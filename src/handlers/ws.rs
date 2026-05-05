use crate::{log, warn};

#[allow(unused_imports)]
use {
    crate::core::sol::TokenInfo,
    crate::core::sol::{SYSTEM_PROGRAM, SolHook, WSOL_MINT},
    crate::dex::meteora_damm_v1::{METEORA_DAMM_V1_ID, MeteoraDammV1, MeteoraDammV1Event},
    crate::dex::meteora_damm_v2::{METEORA_DAMM_V2_ID, MeteoraDammV2, MeteoraDammV2Event},
    crate::dex::meteora_dbc::{METEORA_DBC_ID, MeteoraDbc, MeteoraDbcEvent},
    crate::dex::meteora_dlmm::{METEORA_DLMM_ID, MeteoraDlmm, MeteoraDlmmEvent},
    crate::dex::pump_fun::{BondingCurveAccount, PUMP_FUN_ID, PumpFun, PumpFunEvent, TOTAL_SUPPLY},
    crate::dex::pump_swap::{PUMP_SWAP_ID, PumpSwap, PumpSwapEvent},
    crate::dex::raydium_amm_v4::{RAYDIUM_AMM_V4_ID, RaydiumAmmV4, RaydiumAmmV4Event},
    crate::dex::raydium_clmm::{RAYDIUM_CLMM_ID, RaydiumClmm, RaydiumClmmEvent},
    crate::dex::raydium_cpmm::{RAYDIUM_CPMM_ID, RaydiumCpmm, RaydiumCpmmEvent},
    crate::dex::raydium_launchpad::{
        LAUNCHPAD_TRADE_DIRECTION_BUY, LAUNCHPAD_TRADE_DIRECTION_SELL, RAYDIUM_LAUNCHPAD_DEVNET_ID,
        RAYDIUM_LAUNCHPAD_ID, RaydiumLaunchpad, RaydiumLaunchpadEvent,
    },
    crate::gate::squeeze::Squeezer,
    crate::utils::writing::{Colors, cc},
    anyhow::Context,
    chrono::Utc,
    dashmap::DashMap,
    dashmap::mapref::entry::Entry,
    moka::sync::Cache,
    pump_fun_types::events::{CreateEvent, TradeEvent},
    pump_swap_types::events::{BuyEvent, CreatePoolEvent, SellEvent},
    pump_swap_types::state::Pool,
    ringbuffer::{AllocRingBuffer, RingBuffer},
    solana_address::Address,
    solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config,
    solana_commitment_config::CommitmentConfig,
    solana_keypair::Keypair,
    solana_program::instruction::Instruction,
    solana_pubkey::{Pubkey, pubkey},
    solana_rpc_client_types::config::RpcTransactionLogsFilter,
    solana_rpc_client_types::response::RpcConfirmedTransactionStatusWithSignature,
    solana_signature::Signature,
    solana_transaction_status::{
        EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiMessage,
        UiTransactionTokenBalance, option_serializer::OptionSerializer,
    },
    sqlx::types::Json,
    std::collections::{HashMap, HashSet},
    std::io::{self, Write},
    std::str::FromStr,
    std::sync::Arc,
    tokio::stream,
    tokio_stream::StreamExt,
    yellowstone_grpc_proto::prelude::subscribe_update::UpdateOneof,
    yellowstone_grpc_proto::prelude::{SubscribeUpdateTransactionInfo, TransactionStatusMeta},
};

pub fn timestamp_now() -> f64 {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|error| {
            warn!("system clock before unix epoch: {}", error);
            std::time::Duration::ZERO
        });
    let ts: f64 = time.as_secs() as f64 + f64::from(time.subsec_nanos()) * 1e-9;
    ts
}

fn confirmed_transaction_logs(tx: &EncodedConfirmedTransactionWithStatusMeta) -> Vec<String> {
    let Some(meta) = tx.transaction.meta.as_ref() else {
        return Vec::new();
    };
    match &meta.log_messages {
        OptionSerializer::Some(logs) => logs.clone(),
        OptionSerializer::None | OptionSerializer::Skip => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationConfidence {
    Confirmed,
    Suspected,
}

impl MigrationConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Suspected => "suspected",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logs(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn test_detect_pump_fun_to_pump_swap_migration_from_logs() {
        let lines = logs(&[
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
            "Program log: Instruction: Migrate",
        ]);
        assert!(WsHandler::detect_pump_fun_to_pump_swap_migration(&lines));
    }

    #[test]
    fn test_launchpad_migration_target_from_logs_prefers_cpmm() {
        let lines = logs(&["Program log: Instruction: MigrateToCpswap"]);
        assert_eq!(
            WsHandler::launchpad_migration_target_from_logs(&lines),
            Some(WsHandler::MARKET_RAYDIUM_CPMM)
        );
    }

    #[test]
    fn test_launchpad_migration_target_from_logs_supports_amm() {
        let lines = logs(&["Program log: Instruction: MigrateToAmm"]);
        assert_eq!(
            WsHandler::launchpad_migration_target_from_logs(&lines),
            Some(WsHandler::MARKET_RAYDIUM_AMM_V4)
        );
    }

    #[test]
    fn test_dbc_migration_target_from_logs_maps_v2() {
        let lines = logs(&["Program log: Instruction: MigrationDammV2"]);
        assert_eq!(
            WsHandler::dbc_migration_target_from_logs(&lines),
            Some(WsHandler::MARKET_METEORA_DAMM_V2)
        );
    }

    #[test]
    fn test_dbc_migration_target_from_logs_maps_v1() {
        let lines = logs(&["Program log: Instruction: MigrateMeteoraDammLockLpToken"]);
        assert_eq!(
            WsHandler::dbc_migration_target_from_logs(&lines),
            Some(WsHandler::MARKET_METEORA_DAMM_V1)
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MigrationEvent {
    pub source_market: &'static str,
    pub target_market: &'static str,
    pub migration_signature: String,
    pub migration_slot: u64,
    pub migration_time: f64,
    pub migration_confidence: MigrationConfidence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mint {
    pub mint: Address,
    pub bonding_curve: Pubkey,
    pub price: f64,
    pub highest_price: f64,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub creator: Pubkey,
    pub creator_sold: bool,
    pub creator_token_amount: f64,
    pub buys: i64,
    pub sells: i64,
    pub tx_count: i64,
    pub volume: f64,
    pub liquidity: f64,
    pub is_migrated: bool,
    pub migration_event: Option<MigrationEvent>,
    pub holder_count: i64,
    pub created_time: f64,
    pub last_activity_time: f64,
}

#[derive(Clone)]
pub struct WsHandler {
    pub sol_hook: SolHook,
    pub ws_url: String,
    pub mints: Cache<Pubkey, Mint>,
    pub holder_balances: DashMap<Pubkey, HashMap<Pubkey, f64>>,
    pub holder_delta_cache: Cache<String, Vec<(Pubkey, f64)>>,
    pub seen_mint_signatures: Cache<String, ()>,
    pub migration_context_cache: Cache<String, (u64, f64)>,
    pub token_info_retries: DashMap<Pubkey, TokenInfoRetryState>,
    pub squeezer: Squeezer,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TokenInfoRetryState {
    pub attempts: usize,
    pub next_retry_unix_ms: u64,
}

impl WsHandler {
    const LAUNCHPAD_BOOTSTRAP_SIGNATURE_LIMIT: usize = 50;
    const LAUNCHPAD_BOOTSTRAP_FETCH_LIMIT: usize = 12;
    const LAUNCHPAD_BOOTSTRAP_PROCESS_LIMIT: usize = 4;

    const UNKNOWN_CREATED_TIME: f64 = 0.0;
    const HOLDER_BALANCE_EPSILON: f64 = 1e-9;
    const TOKEN_INFO_RETRY_BASE_MS: u64 = 250;
    const TOKEN_INFO_RETRY_MAX_MS: u64 = 30_000;
    const TOKEN_INFO_RETRY_COOLDOWN_MS: u64 = 180_000;
    const TOKEN_INFO_RETRY_COOLDOWN_THRESHOLD: usize = 10;
    const MARKET_PUMP_FUN: &'static str = "pump_fun";
    const MARKET_PUMP_SWAP: &'static str = "pump_swap";
    const MARKET_RAYDIUM_AMM_V4: &'static str = "raydium_amm_v4";
    const MARKET_RAYDIUM_LAUNCHPAD: &'static str = "raydium_launchpad";
    const MARKET_RAYDIUM_CPMM: &'static str = "raydium_cpmm";
    const MARKET_METEORA_DBC: &'static str = "meteora_dbc";
    const MARKET_METEORA_DAMM_V1: &'static str = "meteora_damm_v1";
    const MARKET_METEORA_DAMM_V2: &'static str = "meteora_damm_v2";
    const LAUNCHPAD_STATUS_MIGRATE_COMPLETE: u8 = 2;

    const LOG_NEEDLE_INSTRUCTION_MIGRATE: &'static str = "instruction: migrate";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_BONDING_CURVE_CREATOR: &'static str =
        "instruction: migratebondingcurvecreator";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_BONDING_CURVE_CREATOR_SNAKE: &'static str =
        "instruction: migrate_bonding_curve_creator";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_TO_AMM: &'static str = "instruction: migratetoamm";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_TO_AMM_SNAKE: &'static str = "instruction: migrate_to_amm";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_TO_CPSWAP: &'static str = "instruction: migratetocpswap";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_TO_CPSWAP_SNAKE: &'static str =
        "instruction: migrate_to_cpswap";
    const LOG_NEEDLE_INSTRUCTION_MIGRATION_DAMM_V2: &'static str = "instruction: migrationdammv2";
    const LOG_NEEDLE_INSTRUCTION_MIGRATION_DAMM_V2_SNAKE: &'static str =
        "instruction: migration_damm_v2";
    const LOG_NEEDLE_INSTRUCTION_MIGRATION_DAMM_V2_CREATE_METADATA: &'static str =
        "instruction: migrationdammv2createmetadata";
    const LOG_NEEDLE_INSTRUCTION_MIGRATION_DAMM_V2_CREATE_METADATA_SNAKE: &'static str =
        "instruction: migration_damm_v2_create_metadata";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM: &'static str =
        "instruction: migratemeteoradamm";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_SNAKE: &'static str =
        "instruction: migrate_meteora_damm";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_CLAIM_LP: &'static str =
        "instruction: migratemeteoradammclaimlptoken";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_CLAIM_LP_SNAKE: &'static str =
        "instruction: migrate_meteora_damm_claim_lp_token";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_LOCK_LP: &'static str =
        "instruction: migratemeteoradammlocklptoken";
    const LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_LOCK_LP_SNAKE: &'static str =
        "instruction: migrate_meteora_damm_lock_lp_token";
    const LOG_NEEDLE_INSTRUCTION_MIGRATION_METEORA_DAMM_CREATE_ME: &'static str =
        "instruction: migrationmeteoradammcreateme";
    const LOG_NEEDLE_INSTRUCTION_MIGRATION_METEORA_DAMM_CREATE_ME_SNAKE: &'static str =
        "instruction: migration_meteora_damm_create_me";

    fn mint_metadata_incomplete(mint: &Mint) -> bool {
        mint.name.trim().is_empty() || mint.symbol.trim().is_empty() || mint.uri.trim().is_empty()
    }

    fn migration_confidence_rank(confidence: MigrationConfidence) -> u8 {
        match confidence {
            MigrationConfidence::Confirmed => 2,
            MigrationConfidence::Suspected => 1,
        }
    }

    fn should_replace_migration_event(current: &MigrationEvent, next: &MigrationEvent) -> bool {
        let current_rank = Self::migration_confidence_rank(current.migration_confidence);
        let next_rank = Self::migration_confidence_rank(next.migration_confidence);
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

    fn apply_migration_event(mint: &mut Mint, event: MigrationEvent) {
        mint.is_migrated = true;
        if let Some(current) = mint.migration_event.as_ref() {
            if Self::should_replace_migration_event(current, &event) {
                mint.migration_event = Some(event);
            }
        } else {
            mint.migration_event = Some(event);
        }
    }

    async fn resolve_signature_context(
        migration_context_cache: &Cache<String, (u64, f64)>,
        sol_hook: SolHook,
        squeezer: Squeezer,
        signature: &Signature,
    ) -> Option<(u64, f64)> {
        let cache_key = signature.to_string();
        if let Some(cached) = migration_context_cache.get(&cache_key) {
            return Some(cached);
        }

        let parsed_tx = squeezer
            .run_result(|| sol_hook.get_transaction_parsed(signature))
            .await
            .ok()?;
        let slot = parsed_tx.slot;
        let time = parsed_tx
            .block_time
            .map(|ts| ts as f64)
            .unwrap_or_else(timestamp_now);
        let context = (slot, time);
        migration_context_cache.insert(cache_key, context);
        Some(context)
    }

    async fn build_migration_event(
        migration_context_cache: &Cache<String, (u64, f64)>,
        sol_hook: SolHook,
        squeezer: Squeezer,
        signature: &Signature,
        source_market: &'static str,
        target_market: &'static str,
        migration_confidence: MigrationConfidence,
    ) -> MigrationEvent {
        let (migration_slot, migration_time) =
            Self::resolve_signature_context(migration_context_cache, sol_hook, squeezer, signature)
                .await
                .unwrap_or((0, timestamp_now()));

        MigrationEvent {
            source_market,
            target_market,
            migration_signature: signature.to_string(),
            migration_slot,
            migration_time,
            migration_confidence,
        }
    }

    fn launchpad_target_market(migrate_type: u8) -> &'static str {
        if migrate_type == 1 {
            Self::MARKET_RAYDIUM_CPMM
        } else {
            Self::MARKET_RAYDIUM_AMM_V4
        }
    }

    fn dbc_target_market(migration_option: u8) -> &'static str {
        if migration_option == 1 {
            Self::MARKET_METEORA_DAMM_V2
        } else {
            Self::MARKET_METEORA_DAMM_V1
        }
    }

    fn token_info_complete(info: &TokenInfo) -> bool {
        !info.name.trim().is_empty()
            && !info.symbol.trim().is_empty()
            && !info.uri.trim().is_empty()
    }

    fn token_info_has_identity(info: &TokenInfo) -> bool {
        !info.name.trim().is_empty()
            || !info.symbol.trim().is_empty()
            || !info.uri.trim().is_empty()
    }

    fn merge_token_info(mint: &mut Mint, info: &TokenInfo) -> bool {
        let mut changed = false;

        let name = info.name.trim();
        if !name.is_empty() && mint.name.trim() != name {
            mint.name = name.to_string();
            changed = true;
        }

        let symbol = info.symbol.trim();
        if !symbol.is_empty() && mint.symbol.trim() != symbol {
            mint.symbol = symbol.to_string();
            changed = true;
        }

        let uri = info.uri.trim();
        if !uri.is_empty() && mint.uri.trim() != uri {
            mint.uri = uri.to_string();
            changed = true;
        }

        if let Some(creator) = info.creator
            && creator != Pubkey::default()
            && mint.creator != creator
        {
            mint.creator = creator;
            changed = true;
        }

        changed
    }

    fn merge_mint_snapshot(existing: &Mint, incoming: &mut Mint) {
        if incoming.name.trim().is_empty() && !existing.name.trim().is_empty() {
            incoming.name = existing.name.clone();
        }
        if incoming.symbol.trim().is_empty() && !existing.symbol.trim().is_empty() {
            incoming.symbol = existing.symbol.clone();
        }
        if incoming.uri.trim().is_empty() && !existing.uri.trim().is_empty() {
            incoming.uri = existing.uri.clone();
        }
        if incoming.creator == Pubkey::default() && existing.creator != Pubkey::default() {
            incoming.creator = existing.creator;
        }
        if incoming.bonding_curve == Pubkey::default()
            && existing.bonding_curve != Pubkey::default()
        {
            incoming.bonding_curve = existing.bonding_curve;
        }

        incoming.highest_price = incoming
            .highest_price
            .max(existing.highest_price)
            .max(incoming.price.max(existing.price));
        incoming.buys = incoming.buys.max(existing.buys);
        incoming.sells = incoming.sells.max(existing.sells);
        incoming.tx_count = incoming
            .tx_count
            .max(existing.tx_count)
            .max(incoming.buys.saturating_add(incoming.sells));
        incoming.volume = incoming.volume.max(existing.volume);
        incoming.holder_count = incoming.holder_count.max(existing.holder_count);

        if incoming.price <= 0.0 && existing.price > 0.0 {
            incoming.price = existing.price;
        }
        if incoming.liquidity <= 0.0 && existing.liquidity > 0.0 {
            incoming.liquidity = existing.liquidity;
        }

        if incoming.created_time <= Self::UNKNOWN_CREATED_TIME {
            incoming.created_time = existing.created_time;
        } else if existing.created_time > Self::UNKNOWN_CREATED_TIME
            && existing.created_time < incoming.created_time
        {
            incoming.created_time = existing.created_time;
        }

        incoming.creator_sold = incoming.creator_sold || existing.creator_sold;
        incoming.creator_token_amount = incoming
            .creator_token_amount
            .max(existing.creator_token_amount);

        match (
            existing.migration_event.as_ref(),
            incoming.migration_event.as_ref(),
        ) {
            (Some(current), Some(next)) => {
                if !Self::should_replace_migration_event(current, next) {
                    incoming.migration_event = Some(current.clone());
                }
            }
            (Some(current), None) => {
                incoming.migration_event = Some(current.clone());
            }
            _ => {}
        }
        incoming.is_migrated =
            incoming.is_migrated || existing.is_migrated || incoming.migration_event.is_some();
        incoming.last_activity_time = incoming.last_activity_time.max(existing.last_activity_time);
    }

    fn logs_contain(haystack: &[String], needle: &str) -> bool {
        haystack
            .iter()
            .any(|line| line.to_ascii_lowercase().contains(needle))
    }

    fn logs_contain_any(haystack: &[String], needles: &[&str]) -> bool {
        needles
            .iter()
            .any(|needle| Self::logs_contain(haystack, needle))
    }

    fn logs_contain_program_invoke(haystack: &[String], program_id: &Pubkey) -> bool {
        let invoke = format!("program {} invoke", program_id).to_ascii_lowercase();
        Self::logs_contain(haystack, &invoke)
    }

    fn detect_pump_fun_to_pump_swap_migration(logs: &[String]) -> bool {
        if !Self::logs_contain_program_invoke(logs, &PUMP_FUN_ID) {
            return false;
        }

        Self::logs_contain_any(
            logs,
            &[
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_BONDING_CURVE_CREATOR,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_BONDING_CURVE_CREATOR_SNAKE,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE,
            ],
        )
    }

    fn launchpad_migration_target_from_logs(logs: &[String]) -> Option<&'static str> {
        if Self::logs_contain_any(
            logs,
            &[
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_TO_CPSWAP,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_TO_CPSWAP_SNAKE,
            ],
        ) {
            return Some(Self::MARKET_RAYDIUM_CPMM);
        }
        if Self::logs_contain_any(
            logs,
            &[
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_TO_AMM,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_TO_AMM_SNAKE,
            ],
        ) {
            return Some(Self::MARKET_RAYDIUM_AMM_V4);
        }
        None
    }

    fn dbc_migration_target_from_logs(logs: &[String]) -> Option<&'static str> {
        if Self::logs_contain_any(
            logs,
            &[
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATION_DAMM_V2,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATION_DAMM_V2_SNAKE,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATION_DAMM_V2_CREATE_METADATA,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATION_DAMM_V2_CREATE_METADATA_SNAKE,
            ],
        ) {
            return Some(Self::MARKET_METEORA_DAMM_V2);
        }

        if Self::logs_contain_any(
            logs,
            &[
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_SNAKE,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_CLAIM_LP,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_CLAIM_LP_SNAKE,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_LOCK_LP,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATE_METEORA_DAMM_LOCK_LP_SNAKE,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATION_METEORA_DAMM_CREATE_ME,
                Self::LOG_NEEDLE_INSTRUCTION_MIGRATION_METEORA_DAMM_CREATE_ME_SNAKE,
            ],
        ) {
            return Some(Self::MARKET_METEORA_DAMM_V1);
        }

        None
    }

    fn now_unix_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn schedule_token_info_retry(
        retries: &DashMap<Pubkey, TokenInfoRetryState>,
        mint: &Pubkey,
        attempts: usize,
        now_unix_ms: u64,
    ) {
        let delay_ms = if attempts >= Self::TOKEN_INFO_RETRY_COOLDOWN_THRESHOLD {
            Self::TOKEN_INFO_RETRY_COOLDOWN_MS
        } else {
            let pow = attempts.min(8) as u32;
            let factor = 1u64 << pow;
            Self::TOKEN_INFO_RETRY_BASE_MS
                .saturating_mul(factor)
                .min(Self::TOKEN_INFO_RETRY_MAX_MS)
        };

        retries.insert(
            *mint,
            TokenInfoRetryState {
                attempts,
                next_retry_unix_ms: now_unix_ms.saturating_add(delay_ms),
            },
        );
    }

    fn insert_mint_snapshot<K>(mints: &Cache<Pubkey, Mint>, key: K, mut mint: Mint)
    where
        K: Into<Pubkey>,
    {
        if mint.mint == WSOL_MINT || mint.mint == Address::default() {
            return;
        }
        let key_pubkey = key.into();
        if let Some(existing) = mints.get(&key_pubkey) {
            Self::merge_mint_snapshot(&existing, &mut mint);
        } else {
            let mint_key = Pubkey::new_from_array(mint.mint.to_bytes());
            if mint_key != key_pubkey
                && let Some(existing) = mints.get(&mint_key)
            {
                Self::merge_mint_snapshot(&existing, &mut mint);
            }
        }
        mint.last_activity_time = timestamp_now().max(mint.last_activity_time);
        mints.insert(key_pubkey, mint);
    }

    fn apply_holder_delta(
        holder_balances: &DashMap<Pubkey, HashMap<Pubkey, f64>>,
        mint: &Pubkey,
        holder: &Pubkey,
        token_delta: f64,
    ) -> i64 {
        if *mint == Pubkey::default()
            || *mint == WSOL_MINT
            || *holder == Pubkey::default()
            || *holder == SYSTEM_PROGRAM
            || token_delta.abs() <= Self::HOLDER_BALANCE_EPSILON
        {
            return holder_balances
                .get(mint)
                .map(|state| state.len() as i64)
                .unwrap_or(0);
        }

        match holder_balances.entry(*mint) {
            Entry::Occupied(mut occupied) => {
                let balances = occupied.get_mut();
                let current = balances.get(holder).copied().unwrap_or(0.0);
                let next = current + token_delta;
                if next > Self::HOLDER_BALANCE_EPSILON {
                    balances.insert(*holder, next);
                } else {
                    balances.remove(holder);
                }
                balances.len() as i64
            }
            Entry::Vacant(vacant) => {
                if token_delta <= Self::HOLDER_BALANCE_EPSILON {
                    return 0;
                }
                let mut balances = HashMap::new();
                balances.insert(*holder, token_delta);
                vacant.insert(balances);
                1
            }
        }
    }

    fn apply_holder_deltas(
        holder_balances: &DashMap<Pubkey, HashMap<Pubkey, f64>>,
        mint: &Pubkey,
        holder_deltas: &[(Pubkey, f64)],
    ) -> Option<i64> {
        if holder_deltas.is_empty() {
            return None;
        }

        let mut holder_count = holder_balances
            .get(mint)
            .map(|state| state.len() as i64)
            .unwrap_or(0);
        for (holder, token_delta) in holder_deltas {
            holder_count = Self::apply_holder_delta(holder_balances, mint, holder, *token_delta);
        }

        Some(holder_count)
    }

    fn extract_signer_pubkeys_from_message(message: &UiMessage) -> HashSet<Pubkey> {
        match message {
            UiMessage::Parsed(parsed) => parsed
                .account_keys
                .iter()
                .filter(|account| account.signer)
                .filter_map(|account| Pubkey::from_str(&account.pubkey).ok())
                .collect(),
            UiMessage::Raw(raw) => {
                let signer_count = raw.header.num_required_signatures as usize;
                raw.account_keys
                    .iter()
                    .take(signer_count)
                    .filter_map(|account| Pubkey::from_str(account).ok())
                    .collect()
            }
        }
    }

    fn owner_from_token_balance(balance: &UiTransactionTokenBalance) -> Option<Pubkey> {
        let owner = match &balance.owner {
            OptionSerializer::Some(owner) => owner.as_str(),
            OptionSerializer::None | OptionSerializer::Skip => return None,
        };

        Pubkey::from_str(owner).ok()
    }

    fn token_account_from_token_balance(
        message: &UiMessage,
        balance: &UiTransactionTokenBalance,
    ) -> Option<Pubkey> {
        let account_index = balance.account_index as usize;
        match message {
            UiMessage::Parsed(parsed) => parsed
                .account_keys
                .get(account_index)
                .and_then(|account| Pubkey::from_str(&account.pubkey).ok()),
            UiMessage::Raw(raw) => raw
                .account_keys
                .get(account_index)
                .and_then(|account| Pubkey::from_str(account).ok()),
        }
    }

    fn ui_token_amount_to_f64(balance: &UiTransactionTokenBalance) -> Option<f64> {
        if let Some(ui_amount) = balance.ui_token_amount.ui_amount
            && ui_amount.is_finite()
        {
            return Some(ui_amount);
        }

        let ui_amount_string = balance.ui_token_amount.ui_amount_string.trim();
        if !ui_amount_string.is_empty()
            && let Ok(parsed) = ui_amount_string.parse::<f64>()
            && parsed.is_finite()
        {
            return Some(parsed);
        }

        let raw_amount = balance.ui_token_amount.amount.parse::<f64>().ok()?;
        let decimals = i32::from(balance.ui_token_amount.decimals);
        Some(raw_amount / 10_f64.powi(decimals))
    }

    async fn infer_holder_deltas_from_signature(
        holder_delta_cache: &Cache<String, Vec<(Pubkey, f64)>>,
        sol_hook: SolHook,
        squeezer: Squeezer,
        signature: &Signature,
        token_mint: &Pubkey,
    ) -> Vec<(Pubkey, f64)> {
        let cache_key = format!("{}:{}", signature, token_mint);
        if let Some(cached) = holder_delta_cache.get(&cache_key) {
            return cached;
        }

        let parsed_tx = match squeezer
            .run_result(|| sol_hook.get_transaction_parsed(signature))
            .await
        {
            Ok(parsed) => parsed,
            Err(_) => return Vec::new(),
        };

        let EncodedTransaction::Json(ui_tx) = &parsed_tx.transaction.transaction else {
            return Vec::new();
        };

        let signer_pubkeys = Self::extract_signer_pubkeys_from_message(&ui_tx.message);

        let Some(meta) = parsed_tx.transaction.meta.as_ref() else {
            return Vec::new();
        };

        let token_mint_text = token_mint.to_string();
        let mut signer_deltas_by_holder: HashMap<Pubkey, f64> = HashMap::new();
        let mut fallback_deltas_by_holder: HashMap<Pubkey, f64> = HashMap::new();

        let apply_balance_delta =
            |balances: &[UiTransactionTokenBalance],
             sign: f64,
             signer_out: &mut HashMap<Pubkey, f64>,
             fallback_out: &mut HashMap<Pubkey, f64>| {
                for balance in balances {
                    if balance.mint != token_mint_text {
                        continue;
                    }

                    let Some(holder) = Self::owner_from_token_balance(balance).or_else(|| {
                        Self::token_account_from_token_balance(&ui_tx.message, balance)
                    }) else {
                        continue;
                    };
                    if holder == Pubkey::default() || holder == SYSTEM_PROGRAM {
                        continue;
                    }

                    let Some(amount) = Self::ui_token_amount_to_f64(balance) else {
                        continue;
                    };

                    let delta = sign * amount;
                    if delta.abs() <= Self::HOLDER_BALANCE_EPSILON {
                        continue;
                    }

                    if signer_pubkeys.contains(&holder) {
                        *signer_out.entry(holder).or_insert(0.0) += delta;
                    } else {
                        *fallback_out.entry(holder).or_insert(0.0) += delta;
                    }
                }
            };

        if let OptionSerializer::Some(pre_balances) = &meta.pre_token_balances {
            apply_balance_delta(
                pre_balances,
                -1.0,
                &mut signer_deltas_by_holder,
                &mut fallback_deltas_by_holder,
            );
        }
        if let OptionSerializer::Some(post_balances) = &meta.post_token_balances {
            apply_balance_delta(
                post_balances,
                1.0,
                &mut signer_deltas_by_holder,
                &mut fallback_deltas_by_holder,
            );
        }

        let selected_deltas_by_holder = if !signer_deltas_by_holder.is_empty() {
            signer_deltas_by_holder
        } else {
            fallback_deltas_by_holder
        };

        let mut holder_deltas = selected_deltas_by_holder
            .into_iter()
            .filter(|(_, delta)| delta.abs() > Self::HOLDER_BALANCE_EPSILON)
            .collect::<Vec<_>>();
        holder_deltas.sort_by(|left, right| left.0.to_string().cmp(&right.0.to_string()));

        holder_delta_cache.insert(cache_key, holder_deltas.clone());
        holder_deltas
    }

    async fn infer_holder_deltas_from_signature_all(
        holder_delta_cache: &Cache<String, Vec<(Pubkey, f64)>>,
        sol_hook: SolHook,
        squeezer: Squeezer,
        signature: &Signature,
        token_mint: &Pubkey,
        excluded_holders: &HashSet<Pubkey>,
    ) -> Vec<(Pubkey, f64)> {
        let mut excluded = excluded_holders
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        excluded.sort();
        let cache_key = format!("all:{}:{}:{}", signature, token_mint, excluded.join(","));
        if let Some(cached) = holder_delta_cache.get(&cache_key) {
            return cached;
        }

        let parsed_tx = match squeezer
            .run_result(|| sol_hook.get_transaction_parsed(signature))
            .await
        {
            Ok(parsed) => parsed,
            Err(_) => return Vec::new(),
        };

        let EncodedTransaction::Json(ui_tx) = &parsed_tx.transaction.transaction else {
            return Vec::new();
        };

        let Some(meta) = parsed_tx.transaction.meta.as_ref() else {
            return Vec::new();
        };

        let token_mint_text = token_mint.to_string();
        let mut deltas_by_holder: HashMap<Pubkey, f64> = HashMap::new();

        let apply_balance_delta =
            |balances: &[UiTransactionTokenBalance], sign: f64, out: &mut HashMap<Pubkey, f64>| {
                for balance in balances {
                    if balance.mint != token_mint_text {
                        continue;
                    }

                    let Some(holder) = Self::owner_from_token_balance(balance).or_else(|| {
                        Self::token_account_from_token_balance(&ui_tx.message, balance)
                    }) else {
                        continue;
                    };
                    if holder == Pubkey::default()
                        || holder == SYSTEM_PROGRAM
                        || excluded_holders.contains(&holder)
                    {
                        continue;
                    }

                    let Some(amount) = Self::ui_token_amount_to_f64(balance) else {
                        continue;
                    };

                    let delta = sign * amount;
                    if delta.abs() <= Self::HOLDER_BALANCE_EPSILON {
                        continue;
                    }

                    *out.entry(holder).or_insert(0.0) += delta;
                }
            };

        if let OptionSerializer::Some(pre_balances) = &meta.pre_token_balances {
            apply_balance_delta(pre_balances, -1.0, &mut deltas_by_holder);
        }
        if let OptionSerializer::Some(post_balances) = &meta.post_token_balances {
            apply_balance_delta(post_balances, 1.0, &mut deltas_by_holder);
        }

        let mut holder_deltas = deltas_by_holder
            .into_iter()
            .filter(|(_, delta)| delta.abs() > Self::HOLDER_BALANCE_EPSILON)
            .collect::<Vec<_>>();
        holder_deltas.sort_by(|left, right| left.0.to_string().cmp(&right.0.to_string()));

        holder_delta_cache.insert(cache_key, holder_deltas.clone());
        holder_deltas
    }

    fn swap_direction_from_holder_deltas(holder_deltas: &[(Pubkey, f64)]) -> i8 {
        let net_delta: f64 = holder_deltas.iter().map(|(_, delta)| *delta).sum();
        if net_delta > Self::HOLDER_BALANCE_EPSILON {
            1
        } else if net_delta < -(Self::HOLDER_BALANCE_EPSILON) {
            -1
        } else {
            0
        }
    }

    fn register_tx_observation(
        seen_mint_signatures: &Cache<String, ()>,
        mint: &mut Mint,
        signature: Option<&Signature>,
    ) {
        let Some(signature) = signature else {
            mint.tx_count = mint.tx_count.max(mint.buys.saturating_add(mint.sells));
            return;
        };

        if mint.mint == WSOL_MINT || mint.mint == Address::default() {
            return;
        }

        let cache_key = format!("{}:{}", signature, mint.mint);
        if !seen_mint_signatures.contains_key(&cache_key) {
            seen_mint_signatures.insert(cache_key, ());
            mint.tx_count = mint.tx_count.saturating_add(1);
        }
        mint.tx_count = mint.tx_count.max(mint.buys.saturating_add(mint.sells));
    }

    pub fn new(sol_hook: SolHook, ws_url: String) -> Self {
        const SQUEEZE_MAX_RPS_ENV: &str = "SQUEEZE_MAX_RPS";
        const DEFAULT_SQUEEZE_MAX_RPS: u64 = 0;

        let max_rps = match std::env::var(SQUEEZE_MAX_RPS_ENV) {
            Ok(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    DEFAULT_SQUEEZE_MAX_RPS
                } else {
                    match trimmed.parse::<u64>() {
                        Ok(value) => value,
                        Err(_) => {
                            log!(
                                cc::YELLOW,
                                "Invalid {}='{}'; using default {}",
                                SQUEEZE_MAX_RPS_ENV,
                                raw,
                                DEFAULT_SQUEEZE_MAX_RPS
                            );
                            DEFAULT_SQUEEZE_MAX_RPS
                        }
                    }
                }
            }
            Err(_) => DEFAULT_SQUEEZE_MAX_RPS,
        };

        Self::with_rps(sol_hook, ws_url, max_rps)
    }

    pub fn with_rps(sol_hook: SolHook, ws_url: String, max_rps: u64) -> Self {
        let mints = Cache::builder().max_capacity(4096).build();
        let holder_delta_cache = Cache::builder().max_capacity(16_384).build();
        let seen_mint_signatures = Cache::builder().max_capacity(131_072).build();
        let migration_context_cache = Cache::builder().max_capacity(8_192).build();
        let squeezer = Squeezer::new(max_rps);
        Self {
            sol_hook,
            ws_url: ws_url.clone(),
            mints,
            holder_balances: DashMap::new(),
            holder_delta_cache,
            seen_mint_signatures,
            migration_context_cache,
            token_info_retries: DashMap::new(),
            squeezer,
        }
    }

    async fn fetch_token_info_with_limit(
        sol_hook: SolHook,
        retries: DashMap<Pubkey, TokenInfoRetryState>,
        squeezer: Squeezer,
        mint: &Pubkey,
    ) -> Option<TokenInfo> {
        let now_unix_ms = Self::now_unix_ms();
        if let Some(state) = retries.get(mint)
            && now_unix_ms < state.next_retry_unix_ms
        {
            return None;
        }

        let attempts = {
            let mut entry = retries
                .entry(*mint)
                .or_insert(TokenInfoRetryState::default());
            entry.attempts = entry.attempts.saturating_add(1);
            entry.attempts
        };

        let sol_clone = sol_hook.clone();
        match squeezer.run_result(|| sol_clone.get_token_info(mint)).await {
            Ok(info) if Self::token_info_has_identity(&info) => {
                if Self::token_info_complete(&info) {
                    retries.remove(mint);
                } else {
                    Self::schedule_token_info_retry(&retries, mint, attempts, now_unix_ms);
                }
                Some(info)
            }
            Ok(_) | Err(_) => {
                Self::schedule_token_info_retry(&retries, mint, attempts, now_unix_ms);
                None
            }
        }
    }

    pub async fn subscribe_ws_pump_fun(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();

        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![PUMP_FUN_ID.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Pump.fun using WS");

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = PumpFun::parse_logs(msg.logs.iter(), Some(&sig));

            for event in events {
                match event {
                    PumpFunEvent::Trade(Some(trade)) => {
                        let mints = self.mints.clone();
                        let holder_balances = self.holder_balances.clone();
                        let holder_delta_cache = self.holder_delta_cache.clone();
                        let seen_mint_signatures = self.seen_mint_signatures.clone();
                        let sol_hook = self.sol_hook.clone();
                        let squeezer = self.squeezer.clone();
                        let retries = self.token_info_retries.clone();
                        let signature_text = sig.clone();

                        tokio::spawn(async move {
                            // wait 2ms so create arm can index the trade
                            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                            let signature = Signature::from_str(&signature_text).ok();

                            let mut mint = if let Some(mut mint) =
                                mints.get(&Address::from(trade.mint.to_bytes()))
                            {
                                if WsHandler::mint_metadata_incomplete(&mint)
                                    && let Some(token_info) =
                                        WsHandler::fetch_token_info_with_limit(
                                            sol_hook.clone(),
                                            retries.clone(),
                                            squeezer.clone(),
                                            &Address::from(trade.mint.to_bytes()),
                                        )
                                        .await
                                    && WsHandler::merge_token_info(&mut mint, &token_info)
                                {
                                    WsHandler::insert_mint_snapshot(
                                        &mints,
                                        Address::from(trade.mint.to_bytes()),
                                        mint.clone(),
                                    );
                                }
                                mint
                            } else {
                                let bonding_curve = PumpFun::derive_bonding_curve(&Address::from(
                                    trade.mint.to_bytes(),
                                ))
                                .await
                                .unwrap_or(Pubkey::default());
                                let token_info = WsHandler::fetch_token_info_with_limit(
                                    sol_hook.clone(),
                                    retries.clone(),
                                    squeezer.clone(),
                                    &Address::from(trade.mint.to_bytes()),
                                )
                                .await
                                .unwrap_or(TokenInfo {
                                    mint: Pubkey::default(),
                                    name: "".to_string(),
                                    symbol: "".to_string(),
                                    uri: "".to_string(),
                                    creator: None,
                                    authority: Pubkey::default(),
                                });
                                let creator = token_info
                                    .creator
                                    .unwrap_or(Pubkey::new_from_array(trade.creator.to_bytes()));
                                let price = PumpFun::get_price(&trade);
                                let mint = Mint {
                                    mint: Address::from(trade.mint.to_bytes()),
                                    bonding_curve,
                                    price,
                                    highest_price: price,
                                    name: token_info.name,
                                    symbol: token_info.symbol,
                                    uri: token_info.uri,
                                    creator: Address::from(creator.to_bytes()),
                                    creator_sold: false,
                                    creator_token_amount: 0.0,
                                    buys: 0,
                                    sells: 0,
                                    tx_count: 0,
                                    volume: 0.0,
                                    liquidity: trade.real_sol_reserves as f64 / 1e9,
                                    is_migrated: false,
                                    migration_event: None,
                                    holder_count: 0,
                                    created_time: Self::UNKNOWN_CREATED_TIME,
                                    last_activity_time: timestamp_now(),
                                };
                                WsHandler::insert_mint_snapshot(
                                    &mints,
                                    Address::from(trade.mint.to_bytes()),
                                    mint.clone(),
                                );
                                mint
                            };

                            let user = trade.user;
                            let price = PumpFun::get_price(&trade);
                            if price > mint.highest_price {
                                mint.highest_price = price;
                            }
                            let creator = mint.creator;

                            if trade.is_buy {
                                mint.buys += 1;
                                mint.volume += trade.sol_amount as f64 / 1e9;
                            } else {
                                mint.sells += 1;
                                mint.volume += trade.sol_amount as f64 / 1e9;
                            }

                            let tok_amt: f64 = trade.token_amount as f64 / 1e6;
                            let holder = Address::from(user.to_bytes());
                            if Address::from(user.to_bytes()) == Address::from(creator.to_bytes()) {
                                if trade.is_buy {
                                    mint.creator_token_amount += tok_amt;
                                } else {
                                    let dev_sell_pct = if mint.creator_token_amount >= tok_amt
                                        && mint.creator_token_amount > 0.0
                                    {
                                        tok_amt / mint.creator_token_amount
                                    } else {
                                        0.0
                                    };
                                    if dev_sell_pct >= 0.5 {
                                        mint.creator_sold = true;
                                    }
                                    mint.creator_token_amount -= tok_amt;
                                    if mint.creator_token_amount <= 0.0 {
                                        mint.creator_sold = true;
                                    }
                                }
                            }
                            let mut holder_count = None;
                            if let Some(signature) = signature.as_ref() {
                                let mut excluded_holders = HashSet::new();
                                if mint.bonding_curve != Pubkey::default() {
                                    excluded_holders.insert(mint.bonding_curve);
                                }
                                let holder_deltas =
                                    WsHandler::infer_holder_deltas_from_signature_all(
                                        &holder_delta_cache,
                                        sol_hook.clone(),
                                        squeezer.clone(),
                                        signature,
                                        &mint.mint,
                                        &excluded_holders,
                                    )
                                    .await;
                                holder_count = WsHandler::apply_holder_deltas(
                                    &holder_balances,
                                    &mint.mint,
                                    &holder_deltas,
                                );
                            }
                            mint.holder_count = if let Some(holder_count) = holder_count {
                                holder_count
                            } else {
                                let holder_delta = if trade.is_buy { tok_amt } else { -tok_amt };
                                WsHandler::apply_holder_delta(
                                    &holder_balances,
                                    &mint.mint,
                                    &holder,
                                    holder_delta,
                                )
                            };

                            mint.price = price;
                            mint.liquidity = trade.real_sol_reserves as f64 / 1e9;
                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(
                                &mints,
                                Address::from(trade.mint.to_bytes()),
                                mint,
                            );
                        })
                    }

                    PumpFunEvent::Create(Some(create)) => {
                        let mints = self.mints.clone();
                        let holder_balances = self.holder_balances.clone();
                        let seen_mint_signatures = self.seen_mint_signatures.clone();
                        let signature = Signature::from_str(&sig).ok();

                        tokio::spawn(async move {
                            let name = create.name.clone();
                            let symbol = create.symbol.clone();
                            let uri = create.uri.clone();
                            let creator = Address::from(create.creator.to_bytes());
                            let ts = timestamp_now();

                            let price = PumpFun::get_open_price(&create);
                            let mut new_mint = Mint {
                                mint: Address::from(create.mint.to_bytes()),
                                bonding_curve: Address::from(create.bonding_curve.to_bytes()),
                                price,
                                highest_price: price,
                                name: name.clone(),
                                symbol: symbol.clone(),
                                uri: uri.clone(),
                                creator: Address::from(creator.to_bytes()),
                                creator_sold: false,
                                creator_token_amount: 0.0,
                                buys: 0,
                                sells: 0,
                                tx_count: 0,
                                volume: 0.0,
                                // Create events don't carry real reserves; virtual SOL is a
                                // better initial liquidity proxy than zero.
                                liquidity: create.virtual_sol_reserves as f64 / 1e9,
                                is_migrated: false,
                                migration_event: None,
                                holder_count: holder_balances
                                    .get(&Address::from(create.mint.to_bytes()))
                                    .map(|state| state.len() as i64)
                                    .unwrap_or(0),
                                created_time: ts,
                                last_activity_time: ts,
                            };

                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut new_mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(
                                &mints,
                                Address::from(create.mint.to_bytes()),
                                new_mint,
                            );
                        })
                    }
                    _ => tokio::spawn(async move {}),
                };
            }
        }
        Ok(())
    }

    // TODO: Index new mints independently of create pool events, so whenever a buy or sell is detected and we don't have this pool in our cache (use moka)
    pub async fn subscribe_ws_pump_swap(&self) -> anyhow::Result<()> {
        // let buy_amount = config().buy_amount;
        let ws_url = self.ws_url.clone();

        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![PUMP_SWAP_ID.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to PumpSwap using WS");

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = PumpSwap::parse_logs(msg.logs.iter(), Some(&sig));
            let raw_logs = msg.logs.clone();

            let mints = self.mints.clone();
            let holder_balances = self.holder_balances.clone();
            let holder_delta_cache = self.holder_delta_cache.clone();
            let seen_mint_signatures = self.seen_mint_signatures.clone();
            let migration_context_cache = self.migration_context_cache.clone();
            let sol_hook = self.sol_hook.clone();
            let retries = self.token_info_retries.clone();
            let squeezer = self.squeezer.clone();
            let pump_fun_migration_in_logs =
                WsHandler::detect_pump_fun_to_pump_swap_migration(&raw_logs);

            tokio::spawn(async move {
                let signature = Signature::from_str(&sig).ok();
                for event in events {
                    match event {
                        PumpSwapEvent::CreatePool(Some(create)) => {
                            if Address::from(create.base_mint.to_bytes()) == WSOL_MINT {
                                continue;
                            }

                            if Address::from(create.quote_mint.to_bytes()) != WSOL_MINT {
                                continue;
                            }

                            let price = PumpSwap::price_from_create(&create);
                            let legacy_amount_match = create.quote_amount_in >= 80990359346
                                && create.base_amount_in == 206900000000000;
                            let migration_event =
                                if pump_fun_migration_in_logs || legacy_amount_match {
                                    if let Some(signature_ref) = signature.as_ref() {
                                        Some(
                                            WsHandler::build_migration_event(
                                                &migration_context_cache,
                                                sol_hook.clone(),
                                                squeezer.clone(),
                                                signature_ref,
                                                Self::MARKET_PUMP_FUN,
                                                Self::MARKET_PUMP_SWAP,
                                                MigrationConfidence::Confirmed,
                                            )
                                            .await,
                                        )
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                };

                            let mut mint = Mint {
                                mint: Address::from(create.base_mint.to_bytes()),
                                bonding_curve: Address::from(create.pool.to_bytes()),
                                price,
                                highest_price: price,
                                name: "".to_string(),
                                symbol: "".to_string(),
                                uri: "".to_string(),
                                creator: Address::from(create.creator.to_bytes()),
                                creator_sold: false,
                                creator_token_amount: 0.0,
                                buys: 0,
                                sells: 0,
                                tx_count: 0,
                                volume: 0.0,
                                liquidity: create.quote_amount_in.max(create.pool_quote_amount)
                                    as f64
                                    / 1e9,
                                is_migrated: migration_event.is_some(),
                                migration_event,
                                holder_count: holder_balances
                                    .get(&Address::from(create.base_mint.to_bytes()))
                                    .map(|state| state.len() as i64)
                                    .unwrap_or(0),
                                created_time: timestamp_now(),
                                last_activity_time: timestamp_now(),
                            };

                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(
                                &mints,
                                Address::from(create.base_mint.to_bytes()),
                                mint,
                            );
                        }
                        PumpSwapEvent::Buy(Some(buy)) => {
                            // Avoid holding a read lock while awaiting/writing to prevent deadlocks.
                            let existing_mint = {
                                mints
                                    .iter()
                                    .find(|(_, m)| {
                                        m.bonding_curve == Address::from(buy.pool.to_bytes())
                                    })
                                    .map(|(_, m)| m.clone())
                            };
                            let mut mint = if let Some(mut m) = existing_mint {
                                if WsHandler::mint_metadata_incomplete(&m)
                                    && let Some(token_info) =
                                        WsHandler::fetch_token_info_with_limit(
                                            sol_hook.clone(),
                                            retries.clone(),
                                            squeezer.clone(),
                                            &Address::from(m.mint.to_bytes()),
                                        )
                                        .await
                                    && WsHandler::merge_token_info(&mut m, &token_info)
                                {
                                    WsHandler::insert_mint_snapshot(
                                        &mints,
                                        Address::from(m.mint.to_bytes()),
                                        m.clone(),
                                    );
                                }
                                m
                            } else {
                                let mint_address = match squeezer
                                    .run_result(|| {
                                        sol_hook.get_mint_from_token_account(Address::from(
                                            buy.user_base_token_account.to_bytes(),
                                        ))
                                    })
                                    .await
                                {
                                    Ok(addr) => addr,
                                    Err(_) => {
                                        continue;
                                    }
                                };
                                if mint_address == Address::default() {
                                    continue;
                                }
                                let token_info = WsHandler::fetch_token_info_with_limit(
                                    sol_hook.clone(),
                                    retries.clone(),
                                    squeezer.clone(),
                                    &Address::from(mint_address.to_bytes()),
                                )
                                .await
                                .unwrap_or(TokenInfo {
                                    mint: Pubkey::default(),
                                    name: "".to_string(),
                                    symbol: "".to_string(),
                                    uri: "".to_string(),
                                    creator: None,
                                    authority: Pubkey::default(),
                                });
                                let creator = token_info
                                    .creator
                                    .unwrap_or(Pubkey::new_from_array(buy.coin_creator.to_bytes()));
                                let mint = Mint {
                                    mint: Address::from(mint_address.to_bytes()),
                                    bonding_curve: Address::from(buy.pool.to_bytes()),
                                    price: 0.0,
                                    highest_price: 0.0,
                                    name: token_info.name,
                                    symbol: token_info.symbol,
                                    uri: token_info.uri,
                                    creator: Address::from(creator.to_bytes()),
                                    creator_sold: false,
                                    creator_token_amount: 0.0,
                                    buys: 0,
                                    sells: 0,
                                    tx_count: 0,
                                    volume: 0.0,
                                    liquidity: 0.0,
                                    is_migrated: false,
                                    migration_event: None,
                                    holder_count: holder_balances
                                        .get(&Address::from(mint_address.to_bytes()))
                                        .map(|state| state.len() as i64)
                                        .unwrap_or(0),
                                    created_time: Self::UNKNOWN_CREATED_TIME,
                                    last_activity_time: timestamp_now(),
                                };
                                WsHandler::insert_mint_snapshot(&mints, mint_address, mint.clone());
                                mint
                            };

                            let user = buy.user;
                            let price = PumpSwap::price_from_buy(&buy);
                            let creator = mint.creator;
                            if price > mint.highest_price {
                                mint.highest_price = price;
                            }
                            mint.buys += 1;
                            mint.volume += buy.quote_amount_in as f64 / 1e9;

                            if Address::from(user.to_bytes()) == Address::from(creator.to_bytes()) {
                                mint.creator_token_amount += buy.base_amount_out as f64 / 1e6;
                            }
                            let holder = Address::from(user.to_bytes());
                            let mut holder_count = None;
                            if let Some(signature) = signature.as_ref() {
                                let mut excluded_holders = HashSet::new();
                                if mint.bonding_curve != Pubkey::default() {
                                    excluded_holders.insert(mint.bonding_curve);
                                }
                                let holder_deltas =
                                    WsHandler::infer_holder_deltas_from_signature_all(
                                        &holder_delta_cache,
                                        sol_hook.clone(),
                                        squeezer.clone(),
                                        signature,
                                        &mint.mint,
                                        &excluded_holders,
                                    )
                                    .await;
                                holder_count = WsHandler::apply_holder_deltas(
                                    &holder_balances,
                                    &mint.mint,
                                    &holder_deltas,
                                );
                            }
                            mint.holder_count = if let Some(holder_count) = holder_count {
                                holder_count
                            } else {
                                WsHandler::apply_holder_delta(
                                    &holder_balances,
                                    &mint.mint,
                                    &holder,
                                    buy.base_amount_out as f64 / 1e6,
                                )
                            };
                            mint.price = price;
                            mint.liquidity = buy.pool_quote_token_reserves as f64 / 1e9;
                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(&mints, mint.mint, mint);
                        }
                        PumpSwapEvent::Sell(Some(sell)) => {
                            // Avoid holding a read lock while awaiting/writing to prevent deadlocks.
                            let existing_mint = {
                                mints
                                    .iter()
                                    .find(|(_, m)| {
                                        m.bonding_curve == Address::from(sell.pool.to_bytes())
                                    })
                                    .map(|(_, m)| m.clone())
                            };
                            let mut mint = if let Some(mut m) = existing_mint {
                                if WsHandler::mint_metadata_incomplete(&m)
                                    && let Some(token_info) =
                                        WsHandler::fetch_token_info_with_limit(
                                            sol_hook.clone(),
                                            retries.clone(),
                                            squeezer.clone(),
                                            &m.mint,
                                        )
                                        .await
                                    && WsHandler::merge_token_info(&mut m, &token_info)
                                {
                                    WsHandler::insert_mint_snapshot(&mints, m.mint, m.clone());
                                }
                                m
                            } else {
                                let mint_address = match squeezer
                                    .run_result(|| {
                                        sol_hook.get_mint_from_token_account(Address::from(
                                            sell.user_base_token_account.to_bytes(),
                                        ))
                                    })
                                    .await
                                {
                                    Ok(addr) => addr,
                                    Err(_) => {
                                        continue;
                                    }
                                };
                                if mint_address == Pubkey::default() {
                                    continue;
                                }
                                let token_info = WsHandler::fetch_token_info_with_limit(
                                    sol_hook.clone(),
                                    retries.clone(),
                                    squeezer.clone(),
                                    &mint_address,
                                )
                                .await
                                .unwrap_or(TokenInfo {
                                    mint: Pubkey::default(),
                                    name: "".to_string(),
                                    symbol: "".to_string(),
                                    uri: "".to_string(),
                                    creator: None,
                                    authority: Pubkey::default(),
                                });
                                let creator = token_info.creator.unwrap_or(Pubkey::new_from_array(
                                    sell.coin_creator.to_bytes(),
                                ));
                                let mint = Mint {
                                    mint: mint_address,
                                    bonding_curve: Address::from(sell.pool.to_bytes()),
                                    price: 0.0,
                                    highest_price: 0.0,
                                    name: token_info.name,
                                    symbol: token_info.symbol,
                                    uri: token_info.uri,
                                    creator: Address::from(creator.to_bytes()),
                                    creator_sold: false,
                                    creator_token_amount: 0.0,
                                    buys: 0,
                                    sells: 0,
                                    tx_count: 0,
                                    volume: 0.0,
                                    liquidity: 0.0,
                                    is_migrated: false,
                                    migration_event: None,
                                    holder_count: holder_balances
                                        .get(&mint_address)
                                        .map(|state| state.len() as i64)
                                        .unwrap_or(0),
                                    created_time: Self::UNKNOWN_CREATED_TIME,
                                    last_activity_time: timestamp_now(),
                                };
                                WsHandler::insert_mint_snapshot(&mints, mint_address, mint.clone());
                                mint
                            };
                            let user = sell.user;
                            let price = PumpSwap::price_from_sell(&sell);
                            let creator = mint.creator;

                            mint.sells += 1;
                            mint.volume += sell.quote_amount_out as f64 / 1e9;

                            let tok_amt = sell.base_amount_in as f64 / 1e6;
                            if Address::from(user.to_bytes()) == Address::from(creator.to_bytes()) {
                                let dev_sell_pct = if mint.creator_token_amount >= tok_amt
                                    && mint.creator_token_amount > 0.0
                                {
                                    tok_amt / mint.creator_token_amount
                                } else {
                                    0.0
                                };
                                if dev_sell_pct >= 0.5 {
                                    mint.creator_sold = true;
                                }
                                mint.creator_token_amount -= tok_amt;
                                if mint.creator_token_amount <= 0.0 {
                                    mint.creator_sold = true;
                                }
                            }
                            let holder = Address::from(user.to_bytes());
                            let mut holder_count = None;
                            if let Some(signature) = signature.as_ref() {
                                let mut excluded_holders = HashSet::new();
                                if mint.bonding_curve != Pubkey::default() {
                                    excluded_holders.insert(mint.bonding_curve);
                                }
                                let holder_deltas =
                                    WsHandler::infer_holder_deltas_from_signature_all(
                                        &holder_delta_cache,
                                        sol_hook.clone(),
                                        squeezer.clone(),
                                        signature,
                                        &mint.mint,
                                        &excluded_holders,
                                    )
                                    .await;
                                holder_count = WsHandler::apply_holder_deltas(
                                    &holder_balances,
                                    &mint.mint,
                                    &holder_deltas,
                                );
                            }
                            mint.holder_count = if let Some(holder_count) = holder_count {
                                holder_count
                            } else {
                                WsHandler::apply_holder_delta(
                                    &holder_balances,
                                    &mint.mint,
                                    &holder,
                                    -tok_amt,
                                )
                            };
                            mint.price = price;
                            mint.liquidity = sell.pool_quote_token_reserves as f64 / 1e9;
                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(&mints, mint.mint, mint);
                        }
                        PumpSwapEvent::Deposit(Some(deposit)) => {
                            let existing_mint = {
                                mints
                                    .iter()
                                    .find(|(_, m)| {
                                        m.bonding_curve == Address::from(deposit.pool.to_bytes())
                                    })
                                    .map(|(_, m)| m.clone())
                            };
                            let Some(mut mint) = existing_mint else {
                                continue;
                            };

                            let price = PumpSwap::price_from_reserves(
                                deposit.pool_base_token_reserves,
                                deposit.pool_quote_token_reserves,
                            );
                            if price.is_finite() && price > 0.0 {
                                if price > mint.highest_price {
                                    mint.highest_price = price;
                                }
                                mint.price = price;
                            }
                            mint.liquidity = deposit.pool_quote_token_reserves as f64 / 1e9;
                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(&mints, mint.mint, mint);
                        }
                        PumpSwapEvent::Withdraw(Some(withdraw)) => {
                            let existing_mint = {
                                mints
                                    .iter()
                                    .find(|(_, m)| {
                                        m.bonding_curve == Address::from(withdraw.pool.to_bytes())
                                    })
                                    .map(|(_, m)| m.clone())
                            };
                            let Some(mut mint) = existing_mint else {
                                continue;
                            };

                            let price = PumpSwap::price_from_reserves(
                                withdraw.pool_base_token_reserves,
                                withdraw.pool_quote_token_reserves,
                            );
                            if price.is_finite() && price > 0.0 {
                                if price > mint.highest_price {
                                    mint.highest_price = price;
                                }
                                mint.price = price;
                            }
                            mint.liquidity = withdraw.pool_quote_token_reserves as f64 / 1e9;
                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(&mints, mint.mint, mint);
                        }
                        _ => {}
                    }
                }
            });
        }
        Ok(())
    }

    pub async fn subscribe_ws_raydium_clmm(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();
        let program_id = crate::core::cluster::raydium_clmm_program_id(self.sol_hook.cluster);
        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![program_id.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Raydium CLMM using WS");

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = RaydiumClmm::parse_logs(msg.logs.iter(), Some(&sig));
            let mints = self.mints.clone();
            let holder_balances = self.holder_balances.clone();
            let holder_delta_cache = self.holder_delta_cache.clone();
            let seen_mint_signatures = self.seen_mint_signatures.clone();
            let sol_hook = self.sol_hook.clone();
            let retries = self.token_info_retries.clone();
            let squeezer = self.squeezer.clone();

            tokio::spawn(async move {
                let clmm = RaydiumClmm::new(Arc::new(Keypair::new()), Arc::new(sol_hook.clone()));
                let signature = Signature::from_str(&sig).ok();

                for event in events {
                    match event {
                        RaydiumClmmEvent::PoolCreated(Some(create)) => {
                            let pool = Pubkey::new_from_array(create.pool_state.to_bytes());
                            let mint_a = Pubkey::new_from_array(create.token_mint_0.to_bytes());
                            let mint_b = Pubkey::new_from_array(create.token_mint_1.to_bytes());
                            let token_mint = if mint_a == WSOL_MINT && mint_b != WSOL_MINT {
                                mint_b
                            } else if mint_b == WSOL_MINT && mint_a != WSOL_MINT {
                                mint_a
                            } else {
                                continue;
                            };

                            let state = match squeezer.run_result(|| clmm.fetch_state(&pool)).await
                            {
                                Ok(state) => state,
                                Err(_) => continue,
                            };
                            let price =
                                RaydiumClmm::price_from_sqrt_price_x64(&state).unwrap_or_default();
                            let token_info = WsHandler::fetch_token_info_with_limit(
                                sol_hook.clone(),
                                retries.clone(),
                                squeezer.clone(),
                                &token_mint,
                            )
                            .await
                            .unwrap_or(TokenInfo {
                                mint: Pubkey::default(),
                                name: "".to_string(),
                                symbol: "".to_string(),
                                uri: "".to_string(),
                                creator: None,
                                authority: Pubkey::default(),
                            });

                            let creator = token_info.creator.unwrap_or(state.owner);
                            let mut mint = Mint {
                                mint: Address::from(token_mint.to_bytes()),
                                bonding_curve: Address::from(pool.to_bytes()),
                                price,
                                highest_price: price,
                                name: token_info.name,
                                symbol: token_info.symbol,
                                uri: token_info.uri,
                                creator: Address::from(creator.to_bytes()),
                                creator_sold: false,
                                creator_token_amount: 0.0,
                                buys: 0,
                                sells: 0,
                                tx_count: 0,
                                volume: 0.0,
                                liquidity: state.liquidity as f64,
                                is_migrated: false,
                                migration_event: None,
                                holder_count: 0,
                                created_time: timestamp_now(),
                                last_activity_time: timestamp_now(),
                            };
                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(
                                &mints,
                                Address::from(token_mint.to_bytes()),
                                mint,
                            );
                        }
                        RaydiumClmmEvent::Swap(Some(swap)) => {
                            let pool = Pubkey::new_from_array(swap.pool_state.to_bytes());
                            let state = match squeezer.run_result(|| clmm.fetch_state(&pool)).await
                            {
                                Ok(state) => state,
                                Err(_) => continue,
                            };
                            let token_mint =
                                if state.mint_a == WSOL_MINT && state.mint_b != WSOL_MINT {
                                    state.mint_b
                                } else if state.mint_b == WSOL_MINT && state.mint_a != WSOL_MINT {
                                    state.mint_a
                                } else {
                                    continue;
                                };
                            let existing_mint = {
                                mints
                                    .iter()
                                    .find(|(_, m)| {
                                        m.bonding_curve == Address::from(pool.to_bytes())
                                    })
                                    .map(|(_, m)| m.clone())
                            };
                            let mut mint = if let Some(mut m) = existing_mint {
                                if WsHandler::mint_metadata_incomplete(&m)
                                    && let Some(token_info) =
                                        WsHandler::fetch_token_info_with_limit(
                                            sol_hook.clone(),
                                            retries.clone(),
                                            squeezer.clone(),
                                            &token_mint,
                                        )
                                        .await
                                    && WsHandler::merge_token_info(&mut m, &token_info)
                                {
                                    WsHandler::insert_mint_snapshot(
                                        &mints,
                                        Address::from(token_mint.to_bytes()),
                                        m.clone(),
                                    );
                                }
                                m
                            } else {
                                let token_info = WsHandler::fetch_token_info_with_limit(
                                    sol_hook.clone(),
                                    retries.clone(),
                                    squeezer.clone(),
                                    &token_mint,
                                )
                                .await
                                .unwrap_or(TokenInfo {
                                    mint: Pubkey::default(),
                                    name: "".to_string(),
                                    symbol: "".to_string(),
                                    uri: "".to_string(),
                                    creator: None,
                                    authority: Pubkey::default(),
                                });
                                let creator = token_info.creator.unwrap_or(state.owner);
                                let mint = Mint {
                                    mint: Address::from(token_mint.to_bytes()),
                                    bonding_curve: Address::from(pool.to_bytes()),
                                    price: 0.0,
                                    highest_price: 0.0,
                                    name: token_info.name,
                                    symbol: token_info.symbol,
                                    uri: token_info.uri,
                                    creator: Address::from(creator.to_bytes()),
                                    creator_sold: false,
                                    creator_token_amount: 0.0,
                                    buys: 0,
                                    sells: 0,
                                    tx_count: 0,
                                    volume: 0.0,
                                    liquidity: state.liquidity as f64,
                                    is_migrated: false,
                                    migration_event: None,
                                    holder_count: 0,
                                    created_time: Self::UNKNOWN_CREATED_TIME,
                                    last_activity_time: timestamp_now(),
                                };
                                WsHandler::insert_mint_snapshot(
                                    &mints,
                                    Address::from(token_mint.to_bytes()),
                                    mint.clone(),
                                );
                                mint
                            };

                            let price = RaydiumClmm::price_from_sqrt_price_x64(&state)
                                .unwrap_or(mint.price);
                            if price > mint.highest_price {
                                mint.highest_price = price;
                            }
                            mint.price = price;
                            mint.liquidity = state.liquidity as f64;

                            let input_mint = if swap.zero_for_one {
                                state.mint_a
                            } else {
                                state.mint_b
                            };
                            let sol_amount = if state.mint_a == WSOL_MINT {
                                swap.amount_0 as f64 / 1e9
                            } else {
                                swap.amount_1 as f64 / 1e9
                            };
                            if input_mint == WSOL_MINT {
                                mint.buys += 1;
                                mint.volume += sol_amount;
                            } else {
                                mint.sells += 1;
                                mint.volume += sol_amount;
                            }

                            if let Some(signature) = signature.as_ref() {
                                let holder_deltas = WsHandler::infer_holder_deltas_from_signature(
                                    &holder_delta_cache,
                                    sol_hook.clone(),
                                    squeezer.clone(),
                                    signature,
                                    &token_mint,
                                )
                                .await;
                                if let Some(holder_count) = WsHandler::apply_holder_deltas(
                                    &holder_balances,
                                    &mint.mint,
                                    &holder_deltas,
                                ) {
                                    mint.holder_count = holder_count;
                                }
                            }

                            WsHandler::register_tx_observation(
                                &seen_mint_signatures,
                                &mut mint,
                                signature.as_ref(),
                            );
                            WsHandler::insert_mint_snapshot(
                                &mints,
                                Address::from(token_mint.to_bytes()),
                                mint,
                            );
                        }
                        _ => {}
                    }
                }
            });
        }

        Ok(())
    }

    pub async fn subscribe_ws_raydium_cpmm(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();
        let program_id = crate::core::cluster::raydium_cpmm_program_id(self.sol_hook.cluster);
        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![program_id.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Raydium CPMM using WS");
        let cpmm_invoke_prefix = format!("Program {} invoke", RAYDIUM_CPMM_ID);

        {
            let handler = self.clone();
            tokio::spawn(async move {
                if let Err(error) = handler.bootstrap_raydium_cpmm_recent_activity().await {
                    warn!("raydium_cpmm bootstrap failed: {}", error);
                }
            });
        }

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = RaydiumCpmm::parse_logs(msg.logs.iter(), Some(&sig));
            let has_invoke = msg
                .logs
                .iter()
                .any(|log| log.starts_with(&cpmm_invoke_prefix));
            let has_relevant_event = events.iter().any(|event| {
                matches!(
                    event,
                    RaydiumCpmmEvent::Swap(_) | RaydiumCpmmEvent::LpChange(_)
                )
            });
            if !has_invoke && !has_relevant_event {
                continue;
            }

            let handler = self.clone();
            tokio::spawn(async move {
                handler.process_raydium_cpmm_observation(sig, events).await;
            });
        }

        Ok(())
    }

    async fn process_raydium_cpmm_observation(&self, sig: String, events: Vec<RaydiumCpmmEvent>) {
        let cpmm = RaydiumCpmm::new(Arc::new(Keypair::new()), Arc::new(self.sol_hook.clone()));

        let signature = match Signature::from_str(&sig) {
            Ok(signature) => signature,
            Err(_) => return,
        };

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
            match self
                .squeezer
                .run_result(|| cpmm.find_pool_from_signature(&signature))
                .await
            {
                Ok(Some(pool)) => pool,
                _ => return,
            }
        };

        let state = match self.squeezer.run_result(|| cpmm.fetch_state(&pool)).await {
            Ok(state) => state,
            Err(_) => return,
        };

        let token_mint = if state.token_0_mint == WSOL_MINT && state.token_1_mint != WSOL_MINT {
            state.token_1_mint
        } else if state.token_1_mint == WSOL_MINT && state.token_0_mint != WSOL_MINT {
            state.token_0_mint
        } else {
            return;
        };

        let price = match self.squeezer.run_result(|| cpmm.fetch_price(&pool)).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => return,
        };
        let liquidity = self
            .squeezer
            .run_result(|| cpmm.fetch_wsol_liquidity_raw(&state))
            .await
            .ok()
            .map(|value| value as f64 / 1e9)
            .unwrap_or(0.0);

        let existing_mint = {
            self.mints
                .iter()
                .find(|(_, mint)| mint.bonding_curve == Address::from(pool.to_bytes()))
                .map(|(_, mint)| mint.clone())
        };

        let mut mint = if let Some(mut cached) = existing_mint {
            if WsHandler::mint_metadata_incomplete(&cached)
                && let Some(token_info) = WsHandler::fetch_token_info_with_limit(
                    self.sol_hook.clone(),
                    self.token_info_retries.clone(),
                    self.squeezer.clone(),
                    &token_mint,
                )
                .await
            {
                WsHandler::merge_token_info(&mut cached, &token_info);
            }
            cached
        } else {
            let token_info = WsHandler::fetch_token_info_with_limit(
                self.sol_hook.clone(),
                self.token_info_retries.clone(),
                self.squeezer.clone(),
                &token_mint,
            )
            .await
            .unwrap_or(TokenInfo {
                mint: Pubkey::default(),
                name: "".to_string(),
                symbol: "".to_string(),
                uri: "".to_string(),
                creator: None,
                authority: Pubkey::default(),
            });

            let creator = token_info.creator.unwrap_or(state.pool_creator);
            Mint {
                mint: Address::from(token_mint.to_bytes()),
                bonding_curve: Address::from(pool.to_bytes()),
                price,
                highest_price: price,
                name: token_info.name,
                symbol: token_info.symbol,
                uri: token_info.uri,
                creator: Address::from(creator.to_bytes()),
                creator_sold: false,
                creator_token_amount: 0.0,
                buys: 0,
                sells: 0,
                tx_count: 0,
                volume: 0.0,
                liquidity,
                is_migrated: false,
                migration_event: None,
                holder_count: 0,
                created_time: Self::UNKNOWN_CREATED_TIME,
                last_activity_time: timestamp_now(),
            }
        };

        if price > mint.highest_price {
            mint.highest_price = price;
        }
        mint.price = price;
        mint.liquidity = liquidity;

        for event in events {
            match event {
                RaydiumCpmmEvent::Swap(Some(swap)) => {
                    if swap.input_mint == WSOL_MINT && swap.output_mint == token_mint {
                        mint.buys += 1;
                        mint.volume += swap.input_amount as f64 / 1e9;
                    } else if swap.input_mint == token_mint && swap.output_mint == WSOL_MINT {
                        mint.sells += 1;
                        mint.volume += swap.output_amount as f64 / 1e9;
                    }
                }
                RaydiumCpmmEvent::LpChange(Some(_)) => {}
                _ => {}
            }
        }

        let holder_deltas = WsHandler::infer_holder_deltas_from_signature(
            &self.holder_delta_cache,
            self.sol_hook.clone(),
            self.squeezer.clone(),
            &signature,
            &token_mint,
        )
        .await;
        if let Some(holder_count) =
            WsHandler::apply_holder_deltas(&self.holder_balances, &mint.mint, &holder_deltas)
        {
            mint.holder_count = holder_count;
        }

        WsHandler::register_tx_observation(&self.seen_mint_signatures, &mut mint, Some(&signature));
        WsHandler::insert_mint_snapshot(&self.mints, Address::from(token_mint.to_bytes()), mint);
    }

    async fn bootstrap_raydium_cpmm_recent_activity(&self) -> anyhow::Result<()> {
        let program_id = crate::core::cluster::raydium_cpmm_program_id(self.sol_hook.cluster);
        let cpmm_invoke_prefix = format!("Program {} invoke", RAYDIUM_CPMM_ID);
        let observations = self
            .collect_recent_program_activity(
                "raydium_cpmm",
                &[program_id],
                Self::LAUNCHPAD_BOOTSTRAP_SIGNATURE_LIMIT,
                Self::LAUNCHPAD_BOOTSTRAP_FETCH_LIMIT,
            )
            .await?;

        let mut processed = 0usize;
        for (signature, raw_logs) in observations {
            if processed >= Self::LAUNCHPAD_BOOTSTRAP_PROCESS_LIMIT {
                break;
            }

            let events = RaydiumCpmm::parse_logs(raw_logs.iter(), Some(&signature));
            let has_invoke = raw_logs
                .iter()
                .any(|log| log.starts_with(&cpmm_invoke_prefix));
            let has_relevant_event = events.iter().any(|event| {
                matches!(
                    event,
                    RaydiumCpmmEvent::Swap(_) | RaydiumCpmmEvent::LpChange(_)
                )
            });
            if !has_invoke && !has_relevant_event {
                continue;
            }

            self.process_raydium_cpmm_observation(signature, events)
                .await;
            processed += 1;
        }

        if processed > 0 {
            log!(
                cc::LIGHT_WHITE,
                "Raydium CPMM bootstrap: processed {} recent transactions",
                processed
            );
        }

        Ok(())
    }

    pub async fn subscribe_ws_raydium_amm_v4(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();
        let program_id = crate::core::cluster::raydium_amm_v4_program_id(self.sol_hook.cluster);
        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![program_id.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Raydium AMM v4 using WS");
        let amm_v4_invoke_prefix = format!("Program {} invoke", RAYDIUM_AMM_V4_ID);

        {
            let handler = self.clone();
            tokio::spawn(async move {
                if let Err(error) = handler.bootstrap_raydium_amm_v4_recent_activity().await {
                    warn!("raydium_amm_v4 bootstrap failed: {}", error);
                }
            });
        }

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = RaydiumAmmV4::parse_logs(msg.logs.iter(), Some(&sig));
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

            let handler = self.clone();
            tokio::spawn(async move {
                handler
                    .process_raydium_amm_v4_observation(sig, events)
                    .await;
            });
        }

        Ok(())
    }

    async fn process_raydium_amm_v4_observation(
        &self,
        sig: String,
        events: Vec<RaydiumAmmV4Event>,
    ) {
        let amm_v4 = RaydiumAmmV4::new(Arc::new(Keypair::new()), Arc::new(self.sol_hook.clone()));

        let signature = match Signature::from_str(&sig) {
            Ok(signature) => signature,
            Err(_) => return,
        };
        let pool = match self
            .squeezer
            .run_result(|| amm_v4.find_pool_from_signature(&signature))
            .await
        {
            Ok(Some(pool)) => pool,
            _ => return,
        };
        let state = match self.squeezer.run_result(|| amm_v4.fetch_state(&pool)).await {
            Ok(state) => state,
            Err(_) => return,
        };

        let token_mint = if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
            state.quote_mint
        } else if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
            state.base_mint
        } else {
            return;
        };

        let price = match self.squeezer.run_result(|| amm_v4.fetch_price(&pool)).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => return,
        };
        let liquidity = self
            .squeezer
            .run_result(|| amm_v4.fetch_wsol_liquidity_raw(&state))
            .await
            .ok()
            .map(|value| value as f64 / 1e9)
            .unwrap_or(0.0);

        let existing_mint = {
            self.mints
                .iter()
                .find(|(_, mint)| mint.bonding_curve == Address::from(pool.to_bytes()))
                .map(|(_, mint)| mint.clone())
        };

        let mut mint = if let Some(mut cached) = existing_mint {
            if WsHandler::mint_metadata_incomplete(&cached)
                && let Some(token_info) = WsHandler::fetch_token_info_with_limit(
                    self.sol_hook.clone(),
                    self.token_info_retries.clone(),
                    self.squeezer.clone(),
                    &token_mint,
                )
                .await
            {
                WsHandler::merge_token_info(&mut cached, &token_info);
            }
            cached
        } else {
            let token_info = WsHandler::fetch_token_info_with_limit(
                self.sol_hook.clone(),
                self.token_info_retries.clone(),
                self.squeezer.clone(),
                &token_mint,
            )
            .await
            .unwrap_or(TokenInfo {
                mint: Pubkey::default(),
                name: "".to_string(),
                symbol: "".to_string(),
                uri: "".to_string(),
                creator: None,
                authority: Pubkey::default(),
            });

            let creator = token_info.creator.unwrap_or(state.owner);
            Mint {
                mint: Address::from(token_mint.to_bytes()),
                bonding_curve: Address::from(pool.to_bytes()),
                price,
                highest_price: price,
                name: token_info.name,
                symbol: token_info.symbol,
                uri: token_info.uri,
                creator: Address::from(creator.to_bytes()),
                creator_sold: false,
                creator_token_amount: 0.0,
                buys: 0,
                sells: 0,
                tx_count: 0,
                volume: 0.0,
                liquidity,
                is_migrated: false,
                migration_event: None,
                holder_count: 0,
                created_time: Self::UNKNOWN_CREATED_TIME,
                last_activity_time: timestamp_now(),
            }
        };

        if price > mint.highest_price {
            mint.highest_price = price;
        }
        mint.price = price;
        mint.liquidity = liquidity;

        let mut parsed_swap_event = false;
        for event in events {
            match event {
                RaydiumAmmV4Event::SwapBaseIn(Some(swap)) => {
                    parsed_swap_event = true;
                    if state.base_mint == WSOL_MINT && state.quote_mint == token_mint {
                        if swap.direction == 2 {
                            mint.buys += 1;
                            mint.volume += swap.amount_in as f64 / 1e9;
                        } else if swap.direction == 1 {
                            mint.sells += 1;
                            mint.volume += swap.out_amount as f64 / 1e9;
                        }
                    } else if state.quote_mint == WSOL_MINT && state.base_mint == token_mint {
                        if swap.direction == 1 {
                            mint.buys += 1;
                            mint.volume += swap.amount_in as f64 / 1e9;
                        } else if swap.direction == 2 {
                            mint.sells += 1;
                            mint.volume += swap.out_amount as f64 / 1e9;
                        }
                    }
                }
                RaydiumAmmV4Event::SwapBaseOut(Some(swap)) => {
                    parsed_swap_event = true;
                    if state.base_mint == WSOL_MINT && state.quote_mint == token_mint {
                        if swap.direction == 2 {
                            mint.buys += 1;
                            mint.volume += swap.deduct_in as f64 / 1e9;
                        } else if swap.direction == 1 {
                            mint.sells += 1;
                            mint.volume += swap.amount_out as f64 / 1e9;
                        }
                    } else if state.quote_mint == WSOL_MINT && state.base_mint == token_mint {
                        if swap.direction == 1 {
                            mint.buys += 1;
                            mint.volume += swap.deduct_in as f64 / 1e9;
                        } else if swap.direction == 2 {
                            mint.sells += 1;
                            mint.volume += swap.amount_out as f64 / 1e9;
                        }
                    }
                }
                _ => {}
            }
        }

        let holder_deltas = WsHandler::infer_holder_deltas_from_signature(
            &self.holder_delta_cache,
            self.sol_hook.clone(),
            self.squeezer.clone(),
            &signature,
            &token_mint,
        )
        .await;

        if !parsed_swap_event {
            match WsHandler::swap_direction_from_holder_deltas(&holder_deltas) {
                1 => mint.buys += 1,
                -1 => mint.sells += 1,
                _ => {
                    if let Ok(Some(input_mint)) = self
                        .squeezer
                        .run_result(|| amm_v4.infer_swap_input_mint_from_signature(&signature))
                        .await
                    {
                        if input_mint == WSOL_MINT {
                            mint.buys += 1;
                        } else if input_mint == token_mint {
                            mint.sells += 1;
                        }
                    }
                }
            }
        }

        if let Some(holder_count) =
            WsHandler::apply_holder_deltas(&self.holder_balances, &mint.mint, &holder_deltas)
        {
            mint.holder_count = holder_count;
        }

        WsHandler::register_tx_observation(&self.seen_mint_signatures, &mut mint, Some(&signature));
        WsHandler::insert_mint_snapshot(&self.mints, Address::from(token_mint.to_bytes()), mint);
    }

    async fn bootstrap_raydium_amm_v4_recent_activity(&self) -> anyhow::Result<()> {
        let program_id = crate::core::cluster::raydium_amm_v4_program_id(self.sol_hook.cluster);
        let amm_v4_invoke_prefix = format!("Program {} invoke", RAYDIUM_AMM_V4_ID);
        let observations = self
            .collect_recent_program_activity("raydium_amm_v4", &[program_id], 200, 80)
            .await?;

        let mut processed = 0usize;
        for (signature, raw_logs) in observations {
            if processed >= 12 {
                break;
            }

            let events = RaydiumAmmV4::parse_logs(raw_logs.iter(), Some(&signature));
            let has_invoke = raw_logs
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

            self.process_raydium_amm_v4_observation(signature, events)
                .await;
            processed += 1;
        }

        if processed > 0 {
            log!(
                cc::LIGHT_WHITE,
                "Raydium AMM v4 bootstrap: processed {} recent transactions",
                processed
            );
        }

        Ok(())
    }

    pub async fn subscribe_ws_raydium_launchpad(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();
        let (mut rx_mainnet, _handle_mainnet) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![RAYDIUM_LAUNCHPAD_ID.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        let (mut rx_devnet, _handle_devnet) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![RAYDIUM_LAUNCHPAD_DEVNET_ID.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Raydium Launchpad using WS");
        let launchpad_invoke_prefixes = [
            format!("Program {} invoke", RAYDIUM_LAUNCHPAD_ID),
            format!("Program {} invoke", RAYDIUM_LAUNCHPAD_DEVNET_ID),
        ];

        {
            let handler = self.clone();
            tokio::spawn(async move {
                if let Err(error) = handler.bootstrap_raydium_launchpad_recent_activity().await {
                    warn!("raydium_launchpad bootstrap failed: {}", error);
                }
            });
        }

        let mut mainnet_closed = false;
        let mut devnet_closed = false;

        while !(mainnet_closed && devnet_closed) {
            let msg = tokio::select! {
                maybe = rx_mainnet.recv(), if !mainnet_closed => match maybe {
                    Some(msg) => Some(msg),
                    None => {
                        mainnet_closed = true;
                        None
                    }
                },
                maybe = rx_devnet.recv(), if !devnet_closed => match maybe {
                    Some(msg) => Some(msg),
                    None => {
                        devnet_closed = true;
                        None
                    }
                },
            };
            let Some(msg) = msg else {
                continue;
            };
            let sig = msg.signature.clone();
            let events = RaydiumLaunchpad::parse_logs(msg.logs.iter(), Some(&sig));
            let raw_logs = msg.logs.clone();
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

            let launchpad_migration_target_from_logs =
                WsHandler::launchpad_migration_target_from_logs(&raw_logs);

            let handler = self.clone();
            tokio::spawn(async move {
                handler
                    .process_raydium_launchpad_observation(
                        sig,
                        events,
                        launchpad_migration_target_from_logs,
                    )
                    .await;
            });
        }

        Ok(())
    }

    async fn process_raydium_launchpad_observation(
        &self,
        sig: String,
        events: Vec<RaydiumLaunchpadEvent>,
        launchpad_migration_target_from_logs: Option<&'static str>,
    ) {
        let launchpad =
            RaydiumLaunchpad::new(Arc::new(Keypair::new()), Arc::new(self.sol_hook.clone()));

        let signature = match Signature::from_str(&sig) {
            Ok(signature) => signature,
            Err(_) => return,
        };

        let mut pool_from_event: Option<Pubkey> = None;
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

        let mut resolved_pool = pool_from_event;
        let mut state = if let Some(pool) = resolved_pool {
            self.squeezer
                .run_result(|| launchpad.fetch_state(&pool))
                .await
                .ok()
        } else {
            None
        };

        if state.is_none() {
            resolved_pool = match self
                .squeezer
                .run_result(|| launchpad.find_pool_from_signature(&signature))
                .await
            {
                Ok(Some(pool)) => Some(pool),
                _ => None,
            };
            if let Some(pool) = resolved_pool {
                state = self
                    .squeezer
                    .run_result(|| launchpad.fetch_state(&pool))
                    .await
                    .ok();
            }
        }

        let (pool, state) = match (resolved_pool, state) {
            (Some(pool), Some(state)) => (pool, state),
            _ => return,
        };

        let token_mint = if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
            state.quote_mint
        } else if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
            state.base_mint
        } else {
            return;
        };

        let price = match self
            .squeezer
            .run_result(|| launchpad.fetch_price(&pool))
            .await
        {
            Ok((_, price)) if price > 0.0 => price,
            _ => return,
        };
        let liquidity = self
            .squeezer
            .run_result(|| launchpad.fetch_wsol_liquidity_raw(&state))
            .await
            .ok()
            .map(|value| value as f64 / 1e9)
            .unwrap_or(0.0);

        let existing_mint = {
            self.mints
                .iter()
                .find(|(_, mint)| mint.bonding_curve == Address::from(pool.to_bytes()))
                .map(|(_, mint)| mint.clone())
        };

        let mut mint = if let Some(mut cached) = existing_mint {
            if WsHandler::mint_metadata_incomplete(&cached)
                && let Some(token_info) = WsHandler::fetch_token_info_with_limit(
                    self.sol_hook.clone(),
                    self.token_info_retries.clone(),
                    self.squeezer.clone(),
                    &token_mint,
                )
                .await
            {
                WsHandler::merge_token_info(&mut cached, &token_info);
            }
            cached
        } else {
            let token_info = WsHandler::fetch_token_info_with_limit(
                self.sol_hook.clone(),
                self.token_info_retries.clone(),
                self.squeezer.clone(),
                &token_mint,
            )
            .await
            .unwrap_or(TokenInfo {
                mint: Pubkey::default(),
                name: "".to_string(),
                symbol: "".to_string(),
                uri: "".to_string(),
                creator: None,
                authority: Pubkey::default(),
            });

            let creator = token_info.creator.unwrap_or(state.creator);
            Mint {
                mint: Address::from(token_mint.to_bytes()),
                bonding_curve: Address::from(pool.to_bytes()),
                price,
                highest_price: price,
                name: token_info.name,
                symbol: token_info.symbol,
                uri: token_info.uri,
                creator: Address::from(creator.to_bytes()),
                creator_sold: false,
                creator_token_amount: 0.0,
                buys: 0,
                sells: 0,
                tx_count: 0,
                volume: 0.0,
                liquidity,
                is_migrated: false,
                migration_event: None,
                holder_count: 0,
                created_time: Self::UNKNOWN_CREATED_TIME,
                last_activity_time: timestamp_now(),
            }
        };

        if price > mint.highest_price {
            mint.highest_price = price;
        }
        mint.price = price;
        mint.liquidity = liquidity;

        let mut parsed_trade_event = false;
        let mut observed_pool_create = false;
        let mut observed_pool_status = state.status;
        for event in events {
            match event {
                RaydiumLaunchpadEvent::Trade(Some(trade)) => {
                    if trade.pool_state != pool {
                        continue;
                    }
                    parsed_trade_event = true;
                    if trade.trade_direction == LAUNCHPAD_TRADE_DIRECTION_BUY {
                        mint.buys += 1;
                        mint.volume += trade.amount_in as f64 / 1e9;
                    } else if trade.trade_direction == LAUNCHPAD_TRADE_DIRECTION_SELL {
                        mint.sells += 1;
                        mint.volume += trade.amount_out as f64 / 1e9;
                    }
                    observed_pool_status = observed_pool_status.max(trade.pool_status);
                }
                RaydiumLaunchpadEvent::PoolCreate(Some(_)) => {
                    observed_pool_create = true;
                }
                _ => {}
            }
        }
        if mint.created_time <= Self::UNKNOWN_CREATED_TIME && observed_pool_create {
            mint.created_time = timestamp_now();
        }

        let holder_deltas = WsHandler::infer_holder_deltas_from_signature(
            &self.holder_delta_cache,
            self.sol_hook.clone(),
            self.squeezer.clone(),
            &signature,
            &token_mint,
        )
        .await;

        if !parsed_trade_event {
            match WsHandler::swap_direction_from_holder_deltas(&holder_deltas) {
                1 => mint.buys += 1,
                -1 => mint.sells += 1,
                _ => {
                    if let Ok(Some(input_mint)) = self
                        .squeezer
                        .run_result(|| launchpad.infer_swap_input_mint_from_signature(&signature))
                        .await
                    {
                        if input_mint == WSOL_MINT {
                            mint.buys += 1;
                        } else if input_mint == token_mint {
                            mint.sells += 1;
                        }
                    }
                }
            }
        }

        if let Some(holder_count) =
            WsHandler::apply_holder_deltas(&self.holder_balances, &mint.mint, &holder_deltas)
        {
            mint.holder_count = holder_count;
        }

        if let Some(target_market) = launchpad_migration_target_from_logs {
            let migration_event = WsHandler::build_migration_event(
                &self.migration_context_cache,
                self.sol_hook.clone(),
                self.squeezer.clone(),
                &signature,
                Self::MARKET_RAYDIUM_LAUNCHPAD,
                target_market,
                MigrationConfidence::Confirmed,
            )
            .await;
            WsHandler::apply_migration_event(&mut mint, migration_event);
        } else if observed_pool_status >= Self::LAUNCHPAD_STATUS_MIGRATE_COMPLETE {
            let target_market = Self::launchpad_target_market(state.migrate_type);
            let migration_event = WsHandler::build_migration_event(
                &self.migration_context_cache,
                self.sol_hook.clone(),
                self.squeezer.clone(),
                &signature,
                Self::MARKET_RAYDIUM_LAUNCHPAD,
                target_market,
                MigrationConfidence::Suspected,
            )
            .await;
            WsHandler::apply_migration_event(&mut mint, migration_event);
        }
        mint.is_migrated = mint.migration_event.is_some();

        WsHandler::register_tx_observation(&self.seen_mint_signatures, &mut mint, Some(&signature));
        WsHandler::insert_mint_snapshot(&self.mints, Address::from(token_mint.to_bytes()), mint);
    }

    async fn bootstrap_raydium_launchpad_recent_activity(&self) -> anyhow::Result<()> {
        let program_ids = [RAYDIUM_LAUNCHPAD_ID, RAYDIUM_LAUNCHPAD_DEVNET_ID];
        let launchpad_invoke_prefixes = [
            format!("Program {} invoke", RAYDIUM_LAUNCHPAD_ID),
            format!("Program {} invoke", RAYDIUM_LAUNCHPAD_DEVNET_ID),
        ];

        let mut merged: HashMap<String, RpcConfirmedTransactionStatusWithSignature> =
            HashMap::new();
        for program_id in program_ids {
            match self
                .sol_hook
                .rpc_client
                .get_signatures_for_address_with_config(
                    &program_id,
                    GetConfirmedSignaturesForAddress2Config {
                        before: None,
                        until: None,
                        limit: Some(Self::LAUNCHPAD_BOOTSTRAP_SIGNATURE_LIMIT),
                        commitment: Some(CommitmentConfig::confirmed()),
                    },
                )
                .await
            {
                Ok(signatures) => {
                    for entry in signatures {
                        merged.entry(entry.signature.clone()).or_insert(entry);
                    }
                }
                Err(error) => {
                    warn!(
                        "raydium_launchpad bootstrap: getSignaturesForAddress({}) failed: {}",
                        program_id, error
                    );
                }
            }
        }

        let mut entries = merged.into_values().collect::<Vec<_>>();
        entries.sort_by(|a, b| b.slot.cmp(&a.slot));

        let mut fetched = 0usize;
        let mut processed = 0usize;
        for entry in entries
            .into_iter()
            .take(Self::LAUNCHPAD_BOOTSTRAP_FETCH_LIMIT)
        {
            if processed >= Self::LAUNCHPAD_BOOTSTRAP_PROCESS_LIMIT {
                break;
            }

            let signature = match Signature::from_str(&entry.signature) {
                Ok(signature) => signature,
                Err(_) => continue,
            };

            let tx = match self.sol_hook.get_transaction_parsed(&signature).await {
                Ok(tx) => tx,
                Err(_) => continue,
            };

            fetched += 1;

            let raw_logs = confirmed_transaction_logs(&tx);
            if raw_logs.is_empty() {
                continue;
            }

            let events = RaydiumLaunchpad::parse_logs(raw_logs.iter(), Some(&entry.signature));
            let has_invoke = raw_logs.iter().any(|log| {
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

            let launchpad_migration_target_from_logs =
                WsHandler::launchpad_migration_target_from_logs(&raw_logs);
            self.process_raydium_launchpad_observation(
                entry.signature,
                events,
                launchpad_migration_target_from_logs,
            )
            .await;
            processed += 1;
        }

        if fetched > 0 {
            log!(
                cc::LIGHT_WHITE,
                "Raydium Launchpad bootstrap: fetched {} txs processed {}",
                fetched,
                processed
            );
        }

        Ok(())
    }

    pub async fn subscribe_ws_meteora_dlmm(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();
        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![METEORA_DLMM_ID.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Meteora DLMM using WS");
        let dlmm_invoke_prefix = format!("Program {} invoke", METEORA_DLMM_ID);

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = MeteoraDlmm::parse_logs(msg.logs.iter(), Some(&sig));
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

            let mints = self.mints.clone();
            let holder_balances = self.holder_balances.clone();
            let holder_delta_cache = self.holder_delta_cache.clone();
            let seen_mint_signatures = self.seen_mint_signatures.clone();
            let sol_hook = self.sol_hook.clone();
            let retries = self.token_info_retries.clone();
            let squeezer = self.squeezer.clone();

            tokio::spawn(async move {
                let dlmm = MeteoraDlmm::new(Arc::new(Keypair::new()), Arc::new(sol_hook.clone()));

                let signature = match Signature::from_str(&sig) {
                    Ok(signature) => signature,
                    Err(_) => return,
                };
                let pool = match squeezer
                    .run_result(|| dlmm.find_pool_from_signature(&signature))
                    .await
                {
                    Ok(Some(pool)) => pool,
                    _ => return,
                };
                let state = match squeezer.run_result(|| dlmm.fetch_state(&pool)).await {
                    Ok(state) => state,
                    Err(_) => return,
                };

                let token_mint =
                    if state.token_x_mint == WSOL_MINT && state.token_y_mint != WSOL_MINT {
                        state.token_y_mint
                    } else if state.token_y_mint == WSOL_MINT && state.token_x_mint != WSOL_MINT {
                        state.token_x_mint
                    } else {
                        return;
                    };

                let price = match squeezer.run_result(|| dlmm.fetch_price(&pool)).await {
                    Ok((_, price)) if price > 0.0 => price,
                    _ => return,
                };

                let wsol_reserve = if state.token_x_mint == WSOL_MINT {
                    state.reserve_x
                } else {
                    state.reserve_y
                };
                let liquidity = squeezer
                    .run_result(|| {
                        sol_hook
                            .rpc_client
                            .get_token_account_balance_with_commitment(
                                &wsol_reserve,
                                CommitmentConfig::confirmed(),
                            )
                    })
                    .await
                    .ok()
                    .and_then(|resp| resp.value.amount.parse::<f64>().ok())
                    .map(|v| v / 1e9)
                    .unwrap_or(0.0);

                let existing_mint = {
                    mints
                        .iter()
                        .find(|(_, m)| m.bonding_curve == Address::from(pool.to_bytes()))
                        .map(|(_, m)| m.clone())
                };

                let mut mint = if let Some(mut cached) = existing_mint {
                    if WsHandler::mint_metadata_incomplete(&cached)
                        && let Some(token_info) = WsHandler::fetch_token_info_with_limit(
                            sol_hook.clone(),
                            retries.clone(),
                            squeezer.clone(),
                            &token_mint,
                        )
                        .await
                    {
                        WsHandler::merge_token_info(&mut cached, &token_info);
                    }
                    cached
                } else {
                    let token_info = WsHandler::fetch_token_info_with_limit(
                        sol_hook.clone(),
                        retries.clone(),
                        squeezer.clone(),
                        &token_mint,
                    )
                    .await
                    .unwrap_or(TokenInfo {
                        mint: Pubkey::default(),
                        name: "".to_string(),
                        symbol: "".to_string(),
                        uri: "".to_string(),
                        creator: None,
                        authority: Pubkey::default(),
                    });

                    let creator = token_info.creator.unwrap_or(state.creator);
                    Mint {
                        mint: Address::from(token_mint.to_bytes()),
                        bonding_curve: Address::from(pool.to_bytes()),
                        price,
                        highest_price: price,
                        name: token_info.name,
                        symbol: token_info.symbol,
                        uri: token_info.uri,
                        creator: Address::from(creator.to_bytes()),
                        creator_sold: false,
                        creator_token_amount: 0.0,
                        buys: 0,
                        sells: 0,
                        tx_count: 0,
                        volume: 0.0,
                        liquidity,
                        is_migrated: false,
                        migration_event: None,
                        holder_count: 0,
                        created_time: Self::UNKNOWN_CREATED_TIME,
                        last_activity_time: timestamp_now(),
                    }
                };

                if price > mint.highest_price {
                    mint.highest_price = price;
                }
                mint.price = price;
                mint.liquidity = liquidity;

                let holder_deltas = WsHandler::infer_holder_deltas_from_signature(
                    &holder_delta_cache,
                    sol_hook.clone(),
                    squeezer.clone(),
                    &signature,
                    &token_mint,
                )
                .await;
                let holder_direction = WsHandler::swap_direction_from_holder_deltas(&holder_deltas);

                let mut observed_pair_create = false;
                for event in events {
                    match event {
                        MeteoraDlmmEvent::Swap(Some(_)) => {
                            if holder_direction > 0 {
                                mint.buys += 1;
                            } else if holder_direction < 0 {
                                mint.sells += 1;
                            }
                        }
                        MeteoraDlmmEvent::LbPairCreate(Some(_)) => {
                            observed_pair_create = true;
                        }
                        _ => {}
                    }
                }
                if mint.created_time <= Self::UNKNOWN_CREATED_TIME && observed_pair_create {
                    mint.created_time = timestamp_now();
                }

                if let Some(holder_count) =
                    WsHandler::apply_holder_deltas(&holder_balances, &mint.mint, &holder_deltas)
                {
                    mint.holder_count = holder_count;
                }

                WsHandler::register_tx_observation(
                    &seen_mint_signatures,
                    &mut mint,
                    Some(&signature),
                );
                WsHandler::insert_mint_snapshot(&mints, Address::from(token_mint.to_bytes()), mint);
            });
        }

        Ok(())
    }

    pub async fn subscribe_ws_meteora_damm_v1(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();
        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![METEORA_DAMM_V1_ID.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Meteora DAMM v1 using WS");
        let damm_invoke_prefix = format!("Program {} invoke", METEORA_DAMM_V1_ID);

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = MeteoraDammV1::parse_logs(msg.logs.iter(), Some(&sig));
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

            let mints = self.mints.clone();
            let holder_balances = self.holder_balances.clone();
            let holder_delta_cache = self.holder_delta_cache.clone();
            let seen_mint_signatures = self.seen_mint_signatures.clone();
            let sol_hook = self.sol_hook.clone();
            let retries = self.token_info_retries.clone();
            let squeezer = self.squeezer.clone();

            tokio::spawn(async move {
                let damm = MeteoraDammV1::new(Arc::new(Keypair::new()), Arc::new(sol_hook.clone()));

                let signature = match Signature::from_str(&sig) {
                    Ok(signature) => signature,
                    Err(_) => return,
                };
                let pool = match squeezer
                    .run_result(|| damm.find_pool_from_signature(&signature))
                    .await
                {
                    Ok(Some(pool)) => pool,
                    _ => return,
                };
                let state = match squeezer.run_result(|| damm.fetch_state(&pool)).await {
                    Ok(state) => state,
                    Err(_) => return,
                };

                let token_mint =
                    if state.token_a_mint == WSOL_MINT && state.token_b_mint != WSOL_MINT {
                        state.token_b_mint
                    } else if state.token_b_mint == WSOL_MINT && state.token_a_mint != WSOL_MINT {
                        state.token_a_mint
                    } else {
                        return;
                    };

                let price = match squeezer.run_result(|| damm.fetch_price(&pool)).await {
                    Ok((_, price)) if price > 0.0 => price,
                    _ => return,
                };
                let liquidity = squeezer
                    .run_result(|| damm.fetch_wsol_liquidity_raw(&state))
                    .await
                    .ok()
                    .map(|v| v as f64 / 1e9)
                    .unwrap_or(0.0);

                let existing_mint = {
                    mints
                        .iter()
                        .find(|(_, m)| m.bonding_curve == Address::from(pool.to_bytes()))
                        .map(|(_, m)| m.clone())
                };

                let mut mint = if let Some(mut cached) = existing_mint {
                    if WsHandler::mint_metadata_incomplete(&cached)
                        && let Some(token_info) = WsHandler::fetch_token_info_with_limit(
                            sol_hook.clone(),
                            retries.clone(),
                            squeezer.clone(),
                            &token_mint,
                        )
                        .await
                    {
                        WsHandler::merge_token_info(&mut cached, &token_info);
                    }
                    cached
                } else {
                    let token_info = WsHandler::fetch_token_info_with_limit(
                        sol_hook.clone(),
                        retries.clone(),
                        squeezer.clone(),
                        &token_mint,
                    )
                    .await
                    .unwrap_or(TokenInfo {
                        mint: Pubkey::default(),
                        name: "".to_string(),
                        symbol: "".to_string(),
                        uri: "".to_string(),
                        creator: None,
                        authority: Pubkey::default(),
                    });

                    let creator = token_info.creator.unwrap_or(Pubkey::default());
                    Mint {
                        mint: Address::from(token_mint.to_bytes()),
                        bonding_curve: Address::from(pool.to_bytes()),
                        price,
                        highest_price: price,
                        name: token_info.name,
                        symbol: token_info.symbol,
                        uri: token_info.uri,
                        creator: Address::from(creator.to_bytes()),
                        creator_sold: false,
                        creator_token_amount: 0.0,
                        buys: 0,
                        sells: 0,
                        tx_count: 0,
                        volume: 0.0,
                        liquidity,
                        is_migrated: false,
                        migration_event: None,
                        holder_count: 0,
                        created_time: Self::UNKNOWN_CREATED_TIME,
                        last_activity_time: timestamp_now(),
                    }
                };

                if price > mint.highest_price {
                    mint.highest_price = price;
                }
                mint.price = price;
                mint.liquidity = liquidity;

                let holder_deltas = WsHandler::infer_holder_deltas_from_signature(
                    &holder_delta_cache,
                    sol_hook.clone(),
                    squeezer.clone(),
                    &signature,
                    &token_mint,
                )
                .await;
                let holder_direction = WsHandler::swap_direction_from_holder_deltas(&holder_deltas);

                let mut observed_pool_create = false;
                for event in events {
                    match event {
                        MeteoraDammV1Event::Swap(Some(_)) => {
                            if holder_direction > 0 {
                                mint.buys += 1;
                            } else if holder_direction < 0 {
                                mint.sells += 1;
                            }
                        }
                        MeteoraDammV1Event::PoolCreated(Some(_)) => {
                            observed_pool_create = true;
                        }
                        _ => {}
                    }
                }
                if mint.created_time <= Self::UNKNOWN_CREATED_TIME && observed_pool_create {
                    mint.created_time = timestamp_now();
                }

                if let Some(holder_count) =
                    WsHandler::apply_holder_deltas(&holder_balances, &mint.mint, &holder_deltas)
                {
                    mint.holder_count = holder_count;
                }

                WsHandler::register_tx_observation(
                    &seen_mint_signatures,
                    &mut mint,
                    Some(&signature),
                );
                WsHandler::insert_mint_snapshot(&mints, Address::from(token_mint.to_bytes()), mint);
            });
        }

        Ok(())
    }

    pub async fn subscribe_ws_meteora_damm_v2(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();
        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![METEORA_DAMM_V2_ID.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Meteora DAMM v2 using WS");
        let damm_invoke_prefix = format!("Program {} invoke", METEORA_DAMM_V2_ID);

        {
            let handler = self.clone();
            tokio::spawn(async move {
                if let Err(error) = handler.bootstrap_meteora_damm_v2_recent_activity().await {
                    warn!("meteora_damm_v2 bootstrap failed: {}", error);
                }
            });
        }

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = MeteoraDammV2::parse_logs(msg.logs.iter(), Some(&sig));
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

            let handler = self.clone();
            tokio::spawn(async move {
                handler
                    .process_meteora_damm_v2_observation(sig, events)
                    .await;
            });
        }

        Ok(())
    }

    async fn process_meteora_damm_v2_observation(
        &self,
        sig: String,
        events: Vec<MeteoraDammV2Event>,
    ) {
        let damm = MeteoraDammV2::new(Arc::new(Keypair::new()), Arc::new(self.sol_hook.clone()));

        let signature = match Signature::from_str(&sig) {
            Ok(signature) => signature,
            Err(_) => return,
        };
        let pool = match self
            .squeezer
            .run_result(|| damm.find_pool_from_signature(&signature))
            .await
        {
            Ok(Some(pool)) => pool,
            _ => return,
        };
        let state = match self.squeezer.run_result(|| damm.fetch_state(&pool)).await {
            Ok(state) => state,
            Err(_) => return,
        };

        let token_mint = if state.token_a_mint == WSOL_MINT && state.token_b_mint != WSOL_MINT {
            state.token_b_mint
        } else if state.token_b_mint == WSOL_MINT && state.token_a_mint != WSOL_MINT {
            state.token_a_mint
        } else {
            return;
        };

        let price = match self.squeezer.run_result(|| damm.fetch_price(&pool)).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => return,
        };
        let liquidity = self
            .squeezer
            .run_result(|| damm.fetch_wsol_liquidity_raw(&state))
            .await
            .ok()
            .map(|v| v as f64 / 1e9)
            .unwrap_or(0.0);

        let existing_mint = {
            self.mints
                .iter()
                .find(|(_, m)| m.bonding_curve == Address::from(pool.to_bytes()))
                .map(|(_, m)| m.clone())
        };

        let mut mint = if let Some(mut cached) = existing_mint {
            if WsHandler::mint_metadata_incomplete(&cached)
                && let Some(token_info) = WsHandler::fetch_token_info_with_limit(
                    self.sol_hook.clone(),
                    self.token_info_retries.clone(),
                    self.squeezer.clone(),
                    &token_mint,
                )
                .await
            {
                WsHandler::merge_token_info(&mut cached, &token_info);
            }
            cached
        } else {
            let token_info = WsHandler::fetch_token_info_with_limit(
                self.sol_hook.clone(),
                self.token_info_retries.clone(),
                self.squeezer.clone(),
                &token_mint,
            )
            .await
            .unwrap_or(TokenInfo {
                mint: Pubkey::default(),
                name: "".to_string(),
                symbol: "".to_string(),
                uri: "".to_string(),
                creator: None,
                authority: Pubkey::default(),
            });

            let creator = token_info.creator.unwrap_or(Pubkey::default());
            Mint {
                mint: Address::from(token_mint.to_bytes()),
                bonding_curve: Address::from(pool.to_bytes()),
                price,
                highest_price: price,
                name: token_info.name,
                symbol: token_info.symbol,
                uri: token_info.uri,
                creator: Address::from(creator.to_bytes()),
                creator_sold: false,
                creator_token_amount: 0.0,
                buys: 0,
                sells: 0,
                tx_count: 0,
                volume: 0.0,
                liquidity,
                is_migrated: false,
                migration_event: None,
                holder_count: 0,
                created_time: Self::UNKNOWN_CREATED_TIME,
                last_activity_time: timestamp_now(),
            }
        };

        if price > mint.highest_price {
            mint.highest_price = price;
        }
        mint.price = price;
        mint.liquidity = liquidity;

        let holder_deltas = WsHandler::infer_holder_deltas_from_signature(
            &self.holder_delta_cache,
            self.sol_hook.clone(),
            self.squeezer.clone(),
            &signature,
            &token_mint,
        )
        .await;
        let holder_direction = WsHandler::swap_direction_from_holder_deltas(&holder_deltas);

        let mut observed_initialize = false;
        for event in events {
            match event {
                MeteoraDammV2Event::Swap2(Some(_)) => {
                    if holder_direction > 0 {
                        mint.buys += 1;
                    } else if holder_direction < 0 {
                        mint.sells += 1;
                    }
                }
                MeteoraDammV2Event::InitializePool(Some(_)) => {
                    observed_initialize = true;
                }
                _ => {}
            }
        }
        if mint.created_time <= Self::UNKNOWN_CREATED_TIME && observed_initialize {
            mint.created_time = timestamp_now();
        }

        if let Some(holder_count) =
            WsHandler::apply_holder_deltas(&self.holder_balances, &mint.mint, &holder_deltas)
        {
            mint.holder_count = holder_count;
        }

        WsHandler::register_tx_observation(&self.seen_mint_signatures, &mut mint, Some(&signature));
        WsHandler::insert_mint_snapshot(&self.mints, Address::from(token_mint.to_bytes()), mint);
    }

    async fn bootstrap_meteora_damm_v2_recent_activity(&self) -> anyhow::Result<()> {
        let damm_invoke_prefix = format!("Program {} invoke", METEORA_DAMM_V2_ID);
        let observations = self
            .collect_recent_program_activity(
                "meteora_damm_v2",
                &[METEORA_DAMM_V2_ID],
                Self::LAUNCHPAD_BOOTSTRAP_SIGNATURE_LIMIT,
                Self::LAUNCHPAD_BOOTSTRAP_FETCH_LIMIT,
            )
            .await?;

        let mut processed = 0usize;
        for (signature, raw_logs) in observations {
            if processed >= Self::LAUNCHPAD_BOOTSTRAP_PROCESS_LIMIT {
                break;
            }

            let events = MeteoraDammV2::parse_logs(raw_logs.iter(), Some(&signature));
            let has_invoke = raw_logs
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

            self.process_meteora_damm_v2_observation(signature, events)
                .await;
            processed += 1;
        }

        if processed > 0 {
            log!(
                cc::LIGHT_WHITE,
                "Meteora DAMM v2 bootstrap: processed {} recent transactions",
                processed
            );
        }

        Ok(())
    }

    pub async fn subscribe_ws_meteora_dbc(&self) -> anyhow::Result<()> {
        let ws_url = self.ws_url.clone();
        let (mut rx, _handle) = self
            .sol_hook
            .subscribe_logs_channel(
                &ws_url,
                RpcTransactionLogsFilter::Mentions(vec![METEORA_DBC_ID.to_string()]),
                CommitmentConfig::processed(),
            )
            .await?;
        log!(cc::LIGHT_WHITE, "Subscribed to Meteora DBC using WS");
        let dbc_invoke_prefix = format!("Program {} invoke", METEORA_DBC_ID);

        {
            let handler = self.clone();
            tokio::spawn(async move {
                if let Err(error) = handler.bootstrap_meteora_dbc_recent_activity().await {
                    warn!("meteora_dbc bootstrap failed: {}", error);
                }
            });
        }

        while let Some(msg) = rx.recv().await {
            let sig = msg.signature.clone();
            let events = MeteoraDbc::parse_logs(msg.logs.iter(), Some(&sig));
            let raw_logs = msg.logs.clone();
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

            let dbc_migration_target_from_logs =
                WsHandler::dbc_migration_target_from_logs(&raw_logs);

            let handler = self.clone();
            tokio::spawn(async move {
                handler
                    .process_meteora_dbc_observation(sig, events, dbc_migration_target_from_logs)
                    .await;
            });
        }

        Ok(())
    }

    async fn process_meteora_dbc_observation(
        &self,
        sig: String,
        events: Vec<MeteoraDbcEvent>,
        dbc_migration_target_from_logs: Option<&'static str>,
    ) {
        let dbc = MeteoraDbc::new(Arc::new(Keypair::new()), Arc::new(self.sol_hook.clone()));

        let signature = match Signature::from_str(&sig) {
            Ok(signature) => signature,
            Err(_) => return,
        };
        let pool = match self
            .squeezer
            .run_result(|| dbc.find_pool_from_signature(&signature))
            .await
        {
            Ok(Some(pool)) => pool,
            _ => return,
        };
        let state = match self.squeezer.run_result(|| dbc.fetch_state(&pool)).await {
            Ok(state) => state,
            Err(_) => return,
        };

        if state.config.quote_mint != WSOL_MINT {
            return;
        }

        let token_mint = state.virtual_pool.base_mint;
        if token_mint == WSOL_MINT || token_mint == Pubkey::default() {
            return;
        }

        let price = match self.squeezer.run_result(|| dbc.fetch_price(&pool)).await {
            Ok((_, price)) if price > 0.0 => price,
            _ => return,
        };
        let liquidity = self
            .squeezer
            .run_result(|| dbc.fetch_wsol_liquidity_raw(&state))
            .await
            .ok()
            .map(|v| v as f64 / 1e9)
            .unwrap_or(0.0);

        let existing_mint = {
            self.mints
                .iter()
                .find(|(_, m)| m.bonding_curve == Address::from(pool.to_bytes()))
                .map(|(_, m)| m.clone())
        };

        let mut mint = if let Some(mut cached) = existing_mint {
            if WsHandler::mint_metadata_incomplete(&cached)
                && let Some(token_info) = WsHandler::fetch_token_info_with_limit(
                    self.sol_hook.clone(),
                    self.token_info_retries.clone(),
                    self.squeezer.clone(),
                    &token_mint,
                )
                .await
            {
                WsHandler::merge_token_info(&mut cached, &token_info);
            }
            cached
        } else {
            let token_info = WsHandler::fetch_token_info_with_limit(
                self.sol_hook.clone(),
                self.token_info_retries.clone(),
                self.squeezer.clone(),
                &token_mint,
            )
            .await
            .unwrap_or(TokenInfo {
                mint: Pubkey::default(),
                name: "".to_string(),
                symbol: "".to_string(),
                uri: "".to_string(),
                creator: None,
                authority: Pubkey::default(),
            });

            let creator = token_info.creator.unwrap_or(state.virtual_pool.creator);
            Mint {
                mint: Address::from(token_mint.to_bytes()),
                bonding_curve: Address::from(pool.to_bytes()),
                price,
                highest_price: price,
                name: token_info.name,
                symbol: token_info.symbol,
                uri: token_info.uri,
                creator: Address::from(creator.to_bytes()),
                creator_sold: false,
                creator_token_amount: 0.0,
                buys: 0,
                sells: 0,
                tx_count: 0,
                volume: 0.0,
                liquidity,
                is_migrated: false,
                migration_event: None,
                holder_count: 0,
                created_time: Self::UNKNOWN_CREATED_TIME,
                last_activity_time: timestamp_now(),
            }
        };

        if price > mint.highest_price {
            mint.highest_price = price;
        }
        mint.price = price;
        mint.liquidity = liquidity;

        let holder_deltas = WsHandler::infer_holder_deltas_from_signature(
            &self.holder_delta_cache,
            self.sol_hook.clone(),
            self.squeezer.clone(),
            &signature,
            &token_mint,
        )
        .await;
        let holder_direction = WsHandler::swap_direction_from_holder_deltas(&holder_deltas);

        let mut observed_initialize = false;
        for event in events {
            match event {
                MeteoraDbcEvent::Swap2(Some(_)) => {
                    if holder_direction > 0 {
                        mint.buys += 1;
                    } else if holder_direction < 0 {
                        mint.sells += 1;
                    }
                }
                MeteoraDbcEvent::InitializePool(Some(_)) => {
                    observed_initialize = true;
                }
                _ => {}
            }
        }
        if mint.created_time <= Self::UNKNOWN_CREATED_TIME && observed_initialize {
            mint.created_time = timestamp_now();
        }

        if let Some(holder_count) =
            WsHandler::apply_holder_deltas(&self.holder_balances, &mint.mint, &holder_deltas)
        {
            mint.holder_count = holder_count;
        }

        if let Some(target_market) = dbc_migration_target_from_logs {
            let migration_event = WsHandler::build_migration_event(
                &self.migration_context_cache,
                self.sol_hook.clone(),
                self.squeezer.clone(),
                &signature,
                Self::MARKET_METEORA_DBC,
                target_market,
                MigrationConfidence::Confirmed,
            )
            .await;
            WsHandler::apply_migration_event(&mut mint, migration_event);
        } else if state.virtual_pool.is_migrated != 0 {
            let target_market = Self::dbc_target_market(state.config.migration_option);
            let migration_event = WsHandler::build_migration_event(
                &self.migration_context_cache,
                self.sol_hook.clone(),
                self.squeezer.clone(),
                &signature,
                Self::MARKET_METEORA_DBC,
                target_market,
                MigrationConfidence::Suspected,
            )
            .await;
            WsHandler::apply_migration_event(&mut mint, migration_event);
        }
        mint.is_migrated = mint.migration_event.is_some();

        WsHandler::register_tx_observation(&self.seen_mint_signatures, &mut mint, Some(&signature));
        WsHandler::insert_mint_snapshot(&self.mints, Address::from(token_mint.to_bytes()), mint);
    }

    async fn bootstrap_meteora_dbc_recent_activity(&self) -> anyhow::Result<()> {
        let dbc_invoke_prefix = format!("Program {} invoke", METEORA_DBC_ID);
        let observations = self
            .collect_recent_program_activity(
                "meteora_dbc",
                &[METEORA_DBC_ID],
                Self::LAUNCHPAD_BOOTSTRAP_SIGNATURE_LIMIT,
                Self::LAUNCHPAD_BOOTSTRAP_FETCH_LIMIT,
            )
            .await?;

        let mut processed = 0usize;
        for (signature, raw_logs) in observations {
            if processed >= Self::LAUNCHPAD_BOOTSTRAP_PROCESS_LIMIT {
                break;
            }

            let events = MeteoraDbc::parse_logs(raw_logs.iter(), Some(&signature));
            let has_invoke = raw_logs
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

            let dbc_migration_target_from_logs =
                WsHandler::dbc_migration_target_from_logs(&raw_logs);
            self.process_meteora_dbc_observation(signature, events, dbc_migration_target_from_logs)
                .await;
            processed += 1;
        }

        if processed > 0 {
            log!(
                cc::LIGHT_WHITE,
                "Meteora DBC bootstrap: processed {} recent transactions",
                processed
            );
        }

        Ok(())
    }

    async fn collect_recent_program_activity(
        &self,
        label: &str,
        program_ids: &[Pubkey],
        signature_limit: usize,
        fetch_limit: usize,
    ) -> anyhow::Result<Vec<(String, Vec<String>)>> {
        let mut merged: HashMap<String, RpcConfirmedTransactionStatusWithSignature> =
            HashMap::new();
        for program_id in program_ids {
            match self
                .sol_hook
                .rpc_client
                .get_signatures_for_address_with_config(
                    program_id,
                    GetConfirmedSignaturesForAddress2Config {
                        before: None,
                        until: None,
                        limit: Some(signature_limit),
                        commitment: Some(CommitmentConfig::confirmed()),
                    },
                )
                .await
            {
                Ok(signatures) => {
                    for entry in signatures {
                        merged.entry(entry.signature.clone()).or_insert(entry);
                    }
                }
                Err(error) => {
                    warn!(
                        "{} bootstrap: getSignaturesForAddress({}) failed: {}",
                        label, program_id, error
                    );
                }
            }
        }

        let mut entries = merged.into_values().collect::<Vec<_>>();
        entries.sort_by(|a, b| b.slot.cmp(&a.slot));

        let mut observations = Vec::new();
        for entry in entries.into_iter().take(fetch_limit) {
            let signature = entry.signature.clone();
            let tx =
                match self
                    .sol_hook
                    .get_transaction_parsed(&Signature::from_str(&signature).with_context(
                        || format!("{label} bootstrap invalid signature: {signature}"),
                    )?)
                    .await
                {
                    Ok(tx) => tx,
                    Err(_) => continue,
                };

            let raw_logs = confirmed_transaction_logs(&tx);
            if raw_logs.is_empty() {
                continue;
            }

            observations.push((signature, raw_logs));
        }

        Ok(observations)
    }
}
