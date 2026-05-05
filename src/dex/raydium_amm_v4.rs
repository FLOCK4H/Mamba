use crate::core::sol::{
    DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SolHook, TOKEN_PROGRAM_ID,
    WSOL_MINT,
};
use crate::utils::utils::decode_b64;
use crate::utils::writing::cc;
use crate::{log, warn};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_system_interface::instruction as system_instruction_if;
use solana_transaction_status::{
    EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiInstruction, UiMessage,
    UiParsedInstruction, option_serializer::OptionSerializer,
};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_associated_token_account::instruction::{
    create_associated_token_account, create_associated_token_account_idempotent,
};
use spl_token::state::Account as SplTokenAccount;
use spl_token_2022::instruction::sync_native;
use spl_token_2022::state::Account as SplToken2022Account;
use std::{collections::BTreeSet, str::FromStr, sync::Arc, time::Duration};

pub const RAYDIUM_AMM_V4_ID: Pubkey = crate::core::cluster::RAYDIUM_AMM_V4_PROGRAM_ID_MAINNET;
pub const RAYDIUM_AMM_V4_DEVNET_ID: Pubkey = crate::core::cluster::RAYDIUM_AMM_V4_PROGRAM_ID_DEVNET;
pub const RAYDIUM_AMM_V4_AUTHORITY_SEED: &[u8] = b"amm authority";
pub const AMM_ASSOCIATED_SEED: &[u8] = b"amm_associated_seed";
pub const TARGET_ASSOCIATED_SEED: &[u8] = b"target_associated_seed";
pub const OPEN_ORDER_ASSOCIATED_SEED: &[u8] = b"open_order_associated_seed";
pub const COIN_VAULT_ASSOCIATED_SEED: &[u8] = b"coin_vault_associated_seed";
pub const PC_VAULT_ASSOCIATED_SEED: &[u8] = b"pc_vault_associated_seed";
pub const LP_MINT_ASSOCIATED_SEED: &[u8] = b"lp_mint_associated_seed";
pub const AMM_CONFIG_SEED: &[u8] = b"amm_config_account_seed";

pub const INITIALIZE2_IX_TAG: u8 = 1;

pub const SWAP_BASE_IN_IX_TAG: u8 = 9;
pub const SWAP_BASE_IN_V2_IX_TAG: u8 = 16;

pub const SEARCH_FOR_RAY_LOG: &str = "ray_log: ";

pub const AMM_V4_POOL_ACCOUNT_LEN: usize = 752;

pub const AMM_V4_POOL_STATUS_OFFSET: usize = 0;
pub const AMM_V4_POOL_NONCE_OFFSET: usize = 8;
pub const AMM_V4_POOL_BASE_DECIMALS_OFFSET: usize = 32;
pub const AMM_V4_POOL_QUOTE_DECIMALS_OFFSET: usize = 40;
pub const AMM_V4_POOL_STATE_OFFSET: usize = 48;
pub const AMM_V4_POOL_RESET_FLAG_OFFSET: usize = 56;
pub const AMM_V4_POOL_TRADE_FEE_NUMERATOR_OFFSET: usize = 144;
pub const AMM_V4_POOL_TRADE_FEE_DENOMINATOR_OFFSET: usize = 152;
pub const AMM_V4_POOL_SWAP_FEE_NUMERATOR_OFFSET: usize = 176;
pub const AMM_V4_POOL_SWAP_FEE_DENOMINATOR_OFFSET: usize = 184;
pub const AMM_V4_POOL_BASE_NEED_TAKE_PNL_OFFSET: usize = 192;
pub const AMM_V4_POOL_QUOTE_NEED_TAKE_PNL_OFFSET: usize = 200;
pub const AMM_V4_POOL_POOL_OPEN_TIME_OFFSET: usize = 224;
pub const AMM_V4_POOL_ORDERBOOK_TO_INIT_TIME_OFFSET: usize = 248;
pub const AMM_V4_POOL_BASE_VAULT_OFFSET: usize = 336;
pub const AMM_V4_POOL_QUOTE_VAULT_OFFSET: usize = 368;
pub const AMM_V4_POOL_BASE_MINT_OFFSET: usize = 400;
pub const AMM_V4_POOL_QUOTE_MINT_OFFSET: usize = 432;
pub const AMM_V4_POOL_LP_MINT_OFFSET: usize = 464;
pub const AMM_V4_POOL_OPEN_ORDERS_OFFSET: usize = 496;
pub const AMM_V4_POOL_MARKET_ID_OFFSET: usize = 528;
pub const AMM_V4_POOL_MARKET_PROGRAM_ID_OFFSET: usize = 560;
pub const AMM_V4_POOL_TARGET_ORDERS_OFFSET: usize = 592;
pub const AMM_V4_POOL_WITHDRAW_QUEUE_OFFSET: usize = 624;
pub const AMM_V4_POOL_LP_VAULT_OFFSET: usize = 656;
pub const AMM_V4_POOL_OWNER_OFFSET: usize = 688;
pub const AMM_V4_POOL_LP_RESERVE_OFFSET: usize = 720;

pub const OPENBOOK_V3_MARKET_MIN_LEN: usize = 388;
pub const OPENBOOK_V3_MARKET_VAULT_SIGNER_NONCE_OFFSET: usize = 45;
pub const OPENBOOK_V3_MARKET_BASE_MINT_OFFSET: usize = 53;
pub const OPENBOOK_V3_MARKET_QUOTE_MINT_OFFSET: usize = 85;
pub const OPENBOOK_V3_MARKET_BASE_VAULT_OFFSET: usize = 117;
pub const OPENBOOK_V3_MARKET_QUOTE_VAULT_OFFSET: usize = 165;
pub const OPENBOOK_V3_MARKET_EVENT_QUEUE_OFFSET: usize = 253;
pub const OPENBOOK_V3_MARKET_BIDS_OFFSET: usize = 285;
pub const OPENBOOK_V3_MARKET_ASKS_OFFSET: usize = 317;

const AMM_STATUS_INITIALIZED: u64 = 1;
const AMM_STATUS_ORDERBOOK_ONLY: u64 = 5;
const AMM_STATUS_SWAP_ONLY: u64 = 6;
const AMM_STATUS_WAITING_TRADE: u64 = 7;

const RAY_LOG_TYPE_SWAP_BASE_IN: u8 = 3;
const RAY_LOG_TYPE_SWAP_BASE_OUT: u8 = 4;

#[derive(Debug, Clone)]
pub struct RaydiumAmmV4PoolState {
    pub status: u64,
    pub nonce: u64,
    pub base_decimals: u64,
    pub quote_decimals: u64,
    pub state: u64,
    pub reset_flag: u64,
    pub trade_fee_numerator: u64,
    pub trade_fee_denominator: u64,
    pub swap_fee_numerator: u64,
    pub swap_fee_denominator: u64,
    pub base_need_take_pnl: u64,
    pub quote_need_take_pnl: u64,
    pub pool_open_time: u64,
    pub orderbook_to_init_time: u64,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub open_orders: Pubkey,
    pub market_id: Pubkey,
    pub market_program_id: Pubkey,
    pub target_orders: Pubkey,
    pub withdraw_queue: Pubkey,
    pub lp_vault: Pubkey,
    pub owner: Pubkey,
    pub lp_reserve: u64,
}

#[derive(Debug, Clone)]
pub struct RaydiumAmmV4MarketState {
    pub vault_signer_nonce: u64,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub event_queue: Pubkey,
    pub bids: Pubkey,
    pub asks: Pubkey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapBaseInRayLog {
    pub log_type: u8,
    pub amount_in: u64,
    pub minimum_out: u64,
    pub direction: u64,
    pub user_source: u64,
    pub pool_coin: u64,
    pub pool_pc: u64,
    pub out_amount: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapBaseOutRayLog {
    pub log_type: u8,
    pub max_in: u64,
    pub amount_out: u64,
    pub direction: u64,
    pub user_source: u64,
    pub pool_coin: u64,
    pub pool_pc: u64,
    pub deduct_in: u64,
}

#[derive(Debug)]
pub enum RaydiumAmmV4Event {
    SwapBaseIn(Option<SwapBaseInRayLog>),
    SwapBaseOut(Option<SwapBaseOutRayLog>),
    Unknown,
}

#[derive(Clone)]
pub struct RaydiumAmmV4 {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
}

impl RaydiumAmmV4 {
    pub fn new(keypair: Arc<Keypair>, sol: Arc<SolHook>) -> Self {
        Self { keypair, sol }
    }

    fn program_id(&self) -> Pubkey {
        match self.sol.cluster {
            crate::core::cluster::SolanaCluster::Devnet => RAYDIUM_AMM_V4_DEVNET_ID,
            _ => RAYDIUM_AMM_V4_ID,
        }
    }

    fn normalize_slippage(slippage: f64) -> f64 {
        let normalized = if slippage > 1.0 {
            slippage / 100.0
        } else {
            slippage
        };
        normalized.clamp(0.01, 0.99)
    }

    fn read_pubkey(data: &[u8], offset: usize) -> anyhow::Result<Pubkey> {
        let bytes = data
            .get(offset..offset + 32)
            .with_context(|| format!("missing pubkey bytes at offset {offset}"))?;
        Ok(Pubkey::new_from_array(bytes.try_into()?))
    }

    fn read_u64(data: &[u8], offset: usize) -> anyhow::Result<u64> {
        let bytes = data
            .get(offset..offset + 8)
            .with_context(|| format!("missing u64 bytes at offset {offset}"))?;
        Ok(u64::from_le_bytes(bytes.try_into()?))
    }

    fn decode_pool_state_account_data(data: &[u8]) -> anyhow::Result<RaydiumAmmV4PoolState> {
        anyhow::ensure!(
            data.len() >= AMM_V4_POOL_ACCOUNT_LEN,
            "raydium amm v4 pool account too short: {}",
            data.len()
        );

        Ok(RaydiumAmmV4PoolState {
            status: Self::read_u64(data, AMM_V4_POOL_STATUS_OFFSET)?,
            nonce: Self::read_u64(data, AMM_V4_POOL_NONCE_OFFSET)?,
            base_decimals: Self::read_u64(data, AMM_V4_POOL_BASE_DECIMALS_OFFSET)?,
            quote_decimals: Self::read_u64(data, AMM_V4_POOL_QUOTE_DECIMALS_OFFSET)?,
            state: Self::read_u64(data, AMM_V4_POOL_STATE_OFFSET)?,
            reset_flag: Self::read_u64(data, AMM_V4_POOL_RESET_FLAG_OFFSET)?,
            trade_fee_numerator: Self::read_u64(data, AMM_V4_POOL_TRADE_FEE_NUMERATOR_OFFSET)?,
            trade_fee_denominator: Self::read_u64(data, AMM_V4_POOL_TRADE_FEE_DENOMINATOR_OFFSET)?,
            swap_fee_numerator: Self::read_u64(data, AMM_V4_POOL_SWAP_FEE_NUMERATOR_OFFSET)?,
            swap_fee_denominator: Self::read_u64(data, AMM_V4_POOL_SWAP_FEE_DENOMINATOR_OFFSET)?,
            base_need_take_pnl: Self::read_u64(data, AMM_V4_POOL_BASE_NEED_TAKE_PNL_OFFSET)?,
            quote_need_take_pnl: Self::read_u64(data, AMM_V4_POOL_QUOTE_NEED_TAKE_PNL_OFFSET)?,
            pool_open_time: Self::read_u64(data, AMM_V4_POOL_POOL_OPEN_TIME_OFFSET)?,
            orderbook_to_init_time: Self::read_u64(
                data,
                AMM_V4_POOL_ORDERBOOK_TO_INIT_TIME_OFFSET,
            )?,
            base_vault: Self::read_pubkey(data, AMM_V4_POOL_BASE_VAULT_OFFSET)?,
            quote_vault: Self::read_pubkey(data, AMM_V4_POOL_QUOTE_VAULT_OFFSET)?,
            base_mint: Self::read_pubkey(data, AMM_V4_POOL_BASE_MINT_OFFSET)?,
            quote_mint: Self::read_pubkey(data, AMM_V4_POOL_QUOTE_MINT_OFFSET)?,
            lp_mint: Self::read_pubkey(data, AMM_V4_POOL_LP_MINT_OFFSET)?,
            open_orders: Self::read_pubkey(data, AMM_V4_POOL_OPEN_ORDERS_OFFSET)?,
            market_id: Self::read_pubkey(data, AMM_V4_POOL_MARKET_ID_OFFSET)?,
            market_program_id: Self::read_pubkey(data, AMM_V4_POOL_MARKET_PROGRAM_ID_OFFSET)?,
            target_orders: Self::read_pubkey(data, AMM_V4_POOL_TARGET_ORDERS_OFFSET)?,
            withdraw_queue: Self::read_pubkey(data, AMM_V4_POOL_WITHDRAW_QUEUE_OFFSET)?,
            lp_vault: Self::read_pubkey(data, AMM_V4_POOL_LP_VAULT_OFFSET)?,
            owner: Self::read_pubkey(data, AMM_V4_POOL_OWNER_OFFSET)?,
            lp_reserve: Self::read_u64(data, AMM_V4_POOL_LP_RESERVE_OFFSET)?,
        })
    }

    fn decode_market_state_account_data(data: &[u8]) -> anyhow::Result<RaydiumAmmV4MarketState> {
        anyhow::ensure!(
            data.len() >= OPENBOOK_V3_MARKET_MIN_LEN,
            "raydium amm v4 market account too short: {}",
            data.len()
        );

        Ok(RaydiumAmmV4MarketState {
            vault_signer_nonce: Self::read_u64(data, OPENBOOK_V3_MARKET_VAULT_SIGNER_NONCE_OFFSET)?,
            base_mint: Self::read_pubkey(data, OPENBOOK_V3_MARKET_BASE_MINT_OFFSET)?,
            quote_mint: Self::read_pubkey(data, OPENBOOK_V3_MARKET_QUOTE_MINT_OFFSET)?,
            base_vault: Self::read_pubkey(data, OPENBOOK_V3_MARKET_BASE_VAULT_OFFSET)?,
            quote_vault: Self::read_pubkey(data, OPENBOOK_V3_MARKET_QUOTE_VAULT_OFFSET)?,
            event_queue: Self::read_pubkey(data, OPENBOOK_V3_MARKET_EVENT_QUEUE_OFFSET)?,
            bids: Self::read_pubkey(data, OPENBOOK_V3_MARKET_BIDS_OFFSET)?,
            asks: Self::read_pubkey(data, OPENBOOK_V3_MARKET_ASKS_OFFSET)?,
        })
    }

    fn encode_swap_base_in_instruction_data(
        amount_in: u64,
        minimum_amount_out: u64,
        use_orderbook_accounts: bool,
    ) -> Vec<u8> {
        let mut data = Vec::with_capacity(1 + 8 + 8);
        data.push(if use_orderbook_accounts {
            SWAP_BASE_IN_IX_TAG
        } else {
            SWAP_BASE_IN_V2_IX_TAG
        });
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&minimum_amount_out.to_le_bytes());
        data
    }

    fn derive_amm_authority_pda(nonce: u64) -> anyhow::Result<Pubkey> {
        Self::derive_amm_authority_pda_for(&RAYDIUM_AMM_V4_ID, nonce)
    }

    fn derive_amm_authority_pda_for(program_id: &Pubkey, nonce: u64) -> anyhow::Result<Pubkey> {
        let nonce_u8 = u8::try_from(nonce).context("raydium amm v4 nonce does not fit u8")?;
        Pubkey::create_program_address(&[RAYDIUM_AMM_V4_AUTHORITY_SEED, &[nonce_u8]], program_id)
            .with_context(|| format!("failed to derive raydium amm v4 authority for nonce {nonce}"))
    }

    fn derive_market_vault_signer(
        market: &Pubkey,
        market_program_id: &Pubkey,
        vault_signer_nonce: u64,
    ) -> anyhow::Result<Pubkey> {
        Pubkey::create_program_address(
            &[market.as_ref(), &vault_signer_nonce.to_le_bytes()],
            market_program_id,
        )
        .with_context(|| {
            format!(
                "failed to derive market vault signer (market={}, market_program={}, nonce={})",
                market, market_program_id, vault_signer_nonce
            )
        })
    }

    fn ata_for(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(owner, mint, token_program)
    }

    fn pool_non_wsol_mint(state: &RaydiumAmmV4PoolState) -> Option<Pubkey> {
        if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
            Some(state.quote_mint)
        } else if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
            Some(state.base_mint)
        } else {
            None
        }
    }

    fn token_decimals_for_mint(state: &RaydiumAmmV4PoolState, mint: &Pubkey) -> anyhow::Result<u8> {
        if state.base_mint == *mint {
            return u8::try_from(state.base_decimals)
                .with_context(|| format!("base decimals out of range: {}", state.base_decimals));
        }
        if state.quote_mint == *mint {
            return u8::try_from(state.quote_decimals)
                .with_context(|| format!("quote decimals out of range: {}", state.quote_decimals));
        }
        anyhow::bail!("mint {} not present in pool", mint)
    }

    fn validate_pool_contains_mint(
        state: &RaydiumAmmV4PoolState,
        mint: &Pubkey,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.base_mint == *mint || state.quote_mint == *mint,
            "raydium amm v4 pool does not contain mint {}",
            mint
        );
        anyhow::ensure!(
            state.base_mint == WSOL_MINT || state.quote_mint == WSOL_MINT,
            "raydium amm v4 pool is not WSOL-quoted"
        );
        Ok(())
    }

    fn validate_swap_enabled(state: &RaydiumAmmV4PoolState) -> anyhow::Result<()> {
        let swap_allowed = matches!(
            state.status,
            AMM_STATUS_INITIALIZED
                | AMM_STATUS_ORDERBOOK_ONLY
                | AMM_STATUS_SWAP_ONLY
                | AMM_STATUS_WAITING_TRADE
        );
        anyhow::ensure!(
            swap_allowed,
            "raydium amm v4 swap is not enabled for status {}",
            state.status
        );
        Ok(())
    }

    fn status_supports_orderbook(status: u64) -> bool {
        matches!(status, AMM_STATUS_INITIALIZED | AMM_STATUS_ORDERBOOK_ONLY)
    }

    fn vault_net_amounts(
        state: &RaydiumAmmV4PoolState,
        base_vault_raw: u64,
        quote_vault_raw: u64,
    ) -> (u64, u64) {
        (
            base_vault_raw.saturating_sub(state.base_need_take_pnl),
            quote_vault_raw.saturating_sub(state.quote_need_take_pnl),
        )
    }

    fn sol_price_from_vault_amounts(
        state: &RaydiumAmmV4PoolState,
        base_vault_net: u64,
        quote_vault_net: u64,
    ) -> anyhow::Result<f64> {
        if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
            let sol_reserve = base_vault_net as f64 / 1e9;
            let token_decimals = u8::try_from(state.quote_decimals).with_context(|| {
                format!("quote decimals out of range: {}", state.quote_decimals)
            })?;
            let token_reserve = quote_vault_net as f64 / 10_f64.powi(token_decimals as i32);
            anyhow::ensure!(token_reserve > 0.0, "raydium amm v4 token reserve is zero");
            let price = sol_reserve / token_reserve;
            anyhow::ensure!(
                price.is_finite() && price > 0.0,
                "invalid raydium amm v4 price"
            );
            return Ok(price);
        }

        if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
            let sol_reserve = quote_vault_net as f64 / 1e9;
            let token_decimals = u8::try_from(state.base_decimals)
                .with_context(|| format!("base decimals out of range: {}", state.base_decimals))?;
            let token_reserve = base_vault_net as f64 / 10_f64.powi(token_decimals as i32);
            anyhow::ensure!(token_reserve > 0.0, "raydium amm v4 token reserve is zero");
            let price = sol_reserve / token_reserve;
            anyhow::ensure!(
                price.is_finite() && price > 0.0,
                "invalid raydium amm v4 price"
            );
            return Ok(price);
        }

        anyhow::bail!("raydium amm v4 pool is not WSOL-quoted");
    }

    async fn account_exists(&self, pubkey: &Pubkey) -> anyhow::Result<bool> {
        Ok(self
            .sol
            .rpc_client
            .get_account_with_commitment(pubkey, CommitmentConfig::processed())
            .await?
            .value
            .is_some())
    }

    async fn token_balance_raw(&self, token_account: &Pubkey) -> anyhow::Result<u64> {
        let balance = self
            .sol
            .rpc_client
            .get_token_account_balance_with_commitment(token_account, CommitmentConfig::confirmed())
            .await
            .with_context(|| {
                format!(
                    "failed to fetch token account balance for {}",
                    token_account
                )
            })?;
        Ok(balance.value.amount.parse::<u64>()?)
    }

    async fn fetch_vault_amounts_raw(
        &self,
        state: &RaydiumAmmV4PoolState,
    ) -> anyhow::Result<(u64, u64)> {
        let base_vault_raw = self.token_balance_raw(&state.base_vault).await?;
        let quote_vault_raw = self.token_balance_raw(&state.quote_vault).await?;
        Ok((base_vault_raw, quote_vault_raw))
    }

    async fn user_token_balance_raw(&self, owner: &Pubkey, mint: &Pubkey) -> anyhow::Result<u64> {
        let ata = Self::ata_for(owner, mint, &TOKEN_PROGRAM_ID);
        self.token_balance_raw(&ata).await
    }

    fn quote_buy_min_output_raw(
        sol_amount_in: f64,
        price: f64,
        slippage_pct: f64,
        mint_decimals: u8,
    ) -> u64 {
        let expected_tokens_out_ui = sol_amount_in / price;
        let min_amount_out = ((expected_tokens_out_ui * (1.0 - slippage_pct)).max(0.0)
            * 10_f64.powi(mint_decimals as i32))
        .floor() as u64;
        min_amount_out.max(1)
    }

    fn quote_sell_min_output_raw(
        amount_in_raw: u64,
        input_decimals: u8,
        price: f64,
        slippage_pct: f64,
    ) -> u64 {
        let amount_in_ui = amount_in_raw as f64 / 10_f64.powi(input_decimals as i32);
        let min_sol_output = amount_in_ui * price * (1.0 - slippage_pct);
        (min_sol_output.max(0.0) * 1e9).floor() as u64
    }

    pub fn parse_logs(
        logs: std::slice::Iter<'_, String>,
        sig: Option<&String>,
    ) -> Vec<RaydiumAmmV4Event> {
        let mut events = Vec::new();
        let sig_text = sig.map(String::as_str).unwrap_or("");

        for log in logs {
            let Some(prefix_idx) = log.find(SEARCH_FOR_RAY_LOG) else {
                continue;
            };
            let payload = log[prefix_idx + SEARCH_FOR_RAY_LOG.len()..].trim();
            let bytes = match decode_b64(payload) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if bytes.is_empty() {
                continue;
            }

            match bytes[0] {
                RAY_LOG_TYPE_SWAP_BASE_IN => {
                    match bincode::deserialize::<SwapBaseInRayLog>(&bytes) {
                        Ok(event) => events.push(RaydiumAmmV4Event::SwapBaseIn(Some(event))),
                        Err(err) => warn!(
                            "Error deserializing raydium amm v4 swap-base-in log {:?}: {err}",
                            sig_text
                        ),
                    }
                }
                RAY_LOG_TYPE_SWAP_BASE_OUT => {
                    match bincode::deserialize::<SwapBaseOutRayLog>(&bytes) {
                        Ok(event) => events.push(RaydiumAmmV4Event::SwapBaseOut(Some(event))),
                        Err(err) => warn!(
                            "Error deserializing raydium amm v4 swap-base-out log {:?}: {err}",
                            sig_text
                        ),
                    }
                }
                _ => events.push(RaydiumAmmV4Event::Unknown),
            }
        }

        events
    }

    pub fn extract_pool_from_transaction(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> Option<Pubkey> {
        fn extract_pool_from_instruction(
            ix: &UiInstruction,
            program_id: &str,
            account_keys: &[&str],
        ) -> Option<Pubkey> {
            match ix {
                UiInstruction::Parsed(UiParsedInstruction::PartiallyDecoded(decoded)) => {
                    if decoded.program_id != program_id {
                        return None;
                    }
                    decoded
                        .accounts
                        .get(1)
                        .and_then(|value| Pubkey::from_str(value).ok())
                }
                UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed)) => {
                    if parsed.program_id != program_id {
                        return None;
                    }
                    let info = parsed.parsed.get("info")?;
                    for key in ["amm", "ammId", "amm_id", "pool"] {
                        if let Some(pool) = info.get(key).and_then(|value| value.as_str())
                            && let Ok(pubkey) = Pubkey::from_str(pool)
                        {
                            return Some(pubkey);
                        }
                    }
                    None
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let pool_idx = *compiled.accounts.get(1)? as usize;
                    let account = account_keys.get(pool_idx)?;
                    Pubkey::from_str(account).ok()
                }
            }
        }

        let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
            return None;
        };

        let UiMessage::Parsed(msg) = &ui_tx.message else {
            return None;
        };

        let program_ids = [
            RAYDIUM_AMM_V4_ID.to_string(),
            RAYDIUM_AMM_V4_DEVNET_ID.to_string(),
        ];
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        for program_id in &program_ids {
            for ix in &msg.instructions {
                if let Some(pool) = extract_pool_from_instruction(ix, program_id, &account_keys) {
                    return Some(pool);
                }
            }
        }

        let Some(meta) = tx.transaction.meta.as_ref() else {
            return None;
        };
        let OptionSerializer::Some(inner_instructions) = &meta.inner_instructions else {
            return None;
        };
        for inner in inner_instructions {
            for program_id in &program_ids {
                for ix in &inner.instructions {
                    if let Some(pool) = extract_pool_from_instruction(ix, program_id, &account_keys)
                    {
                        return Some(pool);
                    }
                }
            }
        }

        None
    }

    pub async fn find_pool_from_signature(
        &self,
        signature: &Signature,
    ) -> anyhow::Result<Option<Pubkey>> {
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..5 {
            match self.sol.get_transaction_parsed(signature).await {
                Ok(tx) => return Ok(Self::extract_pool_from_transaction(&tx)),
                Err(err) => {
                    last_err = Some(err);
                    if attempt < 4 {
                        tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                    }
                }
            }
        }

        Err(last_err
            .unwrap_or_else(|| {
                anyhow::anyhow!("failed to fetch parsed transaction for {signature}")
            })
            .context(format!(
                "failed to fetch parsed transaction for {signature} after retries"
            )))
    }

    pub async fn infer_swap_input_mint_from_signature(
        &self,
        signature: &Signature,
    ) -> anyhow::Result<Option<Pubkey>> {
        fn extract_user_source_account(
            ix: &UiInstruction,
            program_id: &str,
            account_keys: &[&str],
        ) -> Option<Pubkey> {
            match ix {
                UiInstruction::Parsed(UiParsedInstruction::PartiallyDecoded(decoded)) => {
                    if decoded.program_id != program_id || decoded.accounts.len() < 3 {
                        return None;
                    }
                    decoded
                        .accounts
                        .get(decoded.accounts.len() - 3)
                        .and_then(|value| Pubkey::from_str(value).ok())
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id || compiled.accounts.len() < 3 {
                        return None;
                    }
                    let src_idx = *compiled.accounts.get(compiled.accounts.len() - 3)? as usize;
                    let account = account_keys.get(src_idx)?;
                    Pubkey::from_str(account).ok()
                }
                UiInstruction::Parsed(UiParsedInstruction::Parsed(_)) => None,
            }
        }

        let tx = self
            .sol
            .get_transaction_parsed(signature)
            .await
            .with_context(|| format!("failed to fetch parsed transaction for {signature}"))?;

        let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
            return Ok(None);
        };
        let UiMessage::Parsed(msg) = &ui_tx.message else {
            return Ok(None);
        };

        let program_ids = [
            RAYDIUM_AMM_V4_ID.to_string(),
            RAYDIUM_AMM_V4_DEVNET_ID.to_string(),
        ];
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        let mut user_source: Option<Pubkey> = None;
        for program_id in &program_ids {
            for ix in &msg.instructions {
                user_source = extract_user_source_account(ix, program_id, &account_keys);
                if user_source.is_some() {
                    break;
                }
            }
            if user_source.is_some() {
                break;
            }
        }

        if user_source.is_none()
            && let Some(meta) = tx.transaction.meta.as_ref()
            && let OptionSerializer::Some(inner_instructions) = &meta.inner_instructions
        {
            for inner in inner_instructions {
                for program_id in &program_ids {
                    for ix in &inner.instructions {
                        user_source = extract_user_source_account(ix, program_id, &account_keys);
                        if user_source.is_some() {
                            break;
                        }
                    }
                    if user_source.is_some() {
                        break;
                    }
                }
                if user_source.is_some() {
                    break;
                }
            }
        }

        let Some(user_source) = user_source else {
            return Ok(None);
        };

        let token_account = self
            .sol
            .rpc_client
            .get_account_with_commitment(&user_source, CommitmentConfig::confirmed())
            .await?
            .value
            .ok_or(anyhow::anyhow!(
                "user source token account {} not found",
                user_source
            ))?;

        if token_account.owner == TOKEN_PROGRAM_ID {
            let state = SplTokenAccount::unpack(&token_account.data)
                .context("failed to parse SPL token account state")?;
            return Ok(Some(state.mint));
        }
        if token_account.owner == spl_token_2022::id() {
            let state = SplToken2022Account::unpack(&token_account.data)
                .context("failed to parse token-2022 account state")?;
            return Ok(Some(state.mint));
        }

        Ok(None)
    }

    pub async fn fetch_state(&self, pool: &Pubkey) -> anyhow::Result<RaydiumAmmV4PoolState> {
        let data = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("raydium amm v4 pool account not found"))?
            .data;

        Self::decode_pool_state_account_data(&data)
    }

    pub async fn fetch_market_state(
        &self,
        market: &Pubkey,
    ) -> anyhow::Result<RaydiumAmmV4MarketState> {
        let data = self
            .sol
            .rpc_client
            .get_account_with_commitment(market, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("raydium amm v4 market account not found"))?
            .data;

        Self::decode_market_state_account_data(&data)
    }

    pub async fn fetch_wsol_liquidity_raw(
        &self,
        state: &RaydiumAmmV4PoolState,
    ) -> anyhow::Result<u64> {
        let (base_vault_raw, quote_vault_raw) = self.fetch_vault_amounts_raw(state).await?;
        let (base_vault_net, quote_vault_net) =
            Self::vault_net_amounts(state, base_vault_raw, quote_vault_raw);

        if state.base_mint == WSOL_MINT {
            return Ok(base_vault_net);
        }
        if state.quote_mint == WSOL_MINT {
            return Ok(quote_vault_net);
        }
        anyhow::bail!("raydium amm v4 pool quote mint is not WSOL")
    }

    pub async fn fetch_price(&self, pool: &Pubkey) -> anyhow::Result<(RaydiumAmmV4PoolState, f64)> {
        let state = self.fetch_state(pool).await?;
        let (base_vault_raw, quote_vault_raw) = self.fetch_vault_amounts_raw(&state).await?;
        let (base_vault_net, quote_vault_net) =
            Self::vault_net_amounts(&state, base_vault_raw, quote_vault_raw);
        let price = Self::sol_price_from_vault_amounts(&state, base_vault_net, quote_vault_net)?;
        Ok((state, price))
    }

    pub async fn get_mint_from_pool(&self, pool: &Pubkey) -> anyhow::Result<Pubkey> {
        let state = self.fetch_state(pool).await?;
        if let Some(mint) = Self::pool_non_wsol_mint(&state) {
            return Ok(mint);
        }
        Ok(state.base_mint)
    }

    pub async fn find_pools_by_mint(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        let program_id = self.program_id();
        if let Some(quote) = quote_mint {
            let cfg_0 = RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::DataSize(AMM_V4_POOL_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        AMM_V4_POOL_BASE_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        AMM_V4_POOL_QUOTE_MINT_OFFSET,
                        quote.as_ref(),
                    )),
                ]),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::confirmed()),
                    ..Default::default()
                },
                with_context: None,
                sort_results: None,
            };
            let cfg_1 = RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::DataSize(AMM_V4_POOL_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        AMM_V4_POOL_BASE_MINT_OFFSET,
                        quote.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        AMM_V4_POOL_QUOTE_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                ]),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    commitment: Some(CommitmentConfig::confirmed()),
                    ..Default::default()
                },
                with_context: None,
                sort_results: None,
            };

            let pools_0 = self
                .sol
                .rpc_client
                .get_program_ui_accounts_with_config(&program_id, cfg_0)
                .await?;
            let pools_1 = self
                .sol
                .rpc_client
                .get_program_ui_accounts_with_config(&program_id, cfg_1)
                .await?;

            let mut out = BTreeSet::new();
            for (pool, _) in pools_0.into_iter().chain(pools_1.into_iter()) {
                out.insert(pool);
            }
            return Ok(out.into_iter().collect());
        }

        let cfg_base = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(AMM_V4_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    AMM_V4_POOL_BASE_MINT_OFFSET,
                    mint.as_ref(),
                )),
            ]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };
        let cfg_quote = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(AMM_V4_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    AMM_V4_POOL_QUOTE_MINT_OFFSET,
                    mint.as_ref(),
                )),
            ]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };

        let pools_base = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&program_id, cfg_base)
            .await?;
        let pools_quote = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&program_id, cfg_quote)
            .await?;

        let mut out = BTreeSet::new();
        for (pool, _) in pools_base.into_iter().chain(pools_quote.into_iter()) {
            out.insert(pool);
        }

        Ok(out.into_iter().collect())
    }

    pub async fn find_pools_by_owner(&self, owner: &Pubkey) -> anyhow::Result<Vec<Pubkey>> {
        let program_id = self.program_id();
        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(AMM_V4_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    AMM_V4_POOL_OWNER_OFFSET,
                    owner.as_ref(),
                )),
            ]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };

        let pools = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&program_id, cfg)
            .await?;

        Ok(pools.into_iter().map(|(pool, _)| pool).collect())
    }

    pub async fn find_pools_by_lp_mint(&self, lp_mint: &Pubkey) -> anyhow::Result<Vec<Pubkey>> {
        let program_id = self.program_id();
        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(AMM_V4_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    AMM_V4_POOL_LP_MINT_OFFSET,
                    lp_mint.as_ref(),
                )),
            ]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };

        let pools = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&program_id, cfg)
            .await?;

        Ok(pools.into_iter().map(|(pool, _)| pool).collect())
    }

    pub async fn find_pool_by_mint_with_min_liquidity(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
        min_liquidity_raw: u64,
    ) -> anyhow::Result<Option<Pubkey>> {
        let pools = self.find_pools_by_mint(mint, quote_mint).await?;
        let mut best_pool = None;
        let mut best_liquidity = 0u64;

        for pool in pools {
            let state = match self.fetch_state(&pool).await {
                Ok(state) => state,
                Err(_) => continue,
            };
            let liquidity = match self.fetch_wsol_liquidity_raw(&state).await {
                Ok(liquidity) => liquidity,
                Err(_) => continue,
            };
            if liquidity >= min_liquidity_raw && liquidity >= best_liquidity {
                best_liquidity = liquidity;
                best_pool = Some(pool);
            }
        }

        Ok(best_pool)
    }

    pub async fn estimate_withdraw_amounts_raw(
        &self,
        pool: &Pubkey,
        lp_token_amount: u64,
    ) -> anyhow::Result<(RaydiumAmmV4PoolState, u64, u64, u64)> {
        anyhow::ensure!(
            lp_token_amount > 0,
            "raydium amm v4 withdraw lp amount must be > 0"
        );

        let state = self.fetch_state(pool).await?;
        anyhow::ensure!(
            state.lp_reserve > 1,
            "raydium amm v4 pool lp reserve is too small"
        );
        let effective_lp_amount = lp_token_amount.min(state.lp_reserve.saturating_sub(1));
        anyhow::ensure!(
            effective_lp_amount > 0,
            "raydium amm v4 effective withdraw lp amount is zero"
        );

        let (base_vault_raw, quote_vault_raw) = self.fetch_vault_amounts_raw(&state).await?;
        let (base_vault_net, quote_vault_net) =
            Self::vault_net_amounts(&state, base_vault_raw, quote_vault_raw);

        let base_out = ((u128::from(base_vault_net) * u128::from(effective_lp_amount))
            / u128::from(state.lp_reserve)) as u64;
        let quote_out = ((u128::from(quote_vault_net) * u128::from(effective_lp_amount))
            / u128::from(state.lp_reserve)) as u64;
        anyhow::ensure!(
            base_out > 0 || quote_out > 0,
            "raydium amm v4 withdraw quote resulted in zero outputs"
        );

        Ok((state, effective_lp_amount, base_out, quote_out))
    }

    pub async fn withdraw_for_user(
        &self,
        owner: &Pubkey,
        pool: &Pubkey,
        lp_token_amount: u64,
        min_base_amount_out: u64,
        min_quote_amount_out: u64,
    ) -> anyhow::Result<(Vec<Instruction>, RaydiumAmmV4PoolState, u64, u64, u64)> {
        let owner = *owner;
        let program_id = self.program_id();
        let (state, effective_lp_amount, base_out, quote_out) = self
            .estimate_withdraw_amounts_raw(pool, lp_token_amount)
            .await?;
        let authority = Self::derive_amm_authority_pda_for(&program_id, state.nonce)?;
        let market_state = self.fetch_market_state(&state.market_id).await?;
        let market_vault_signer = Self::derive_market_vault_signer(
            &state.market_id,
            &state.market_program_id,
            market_state.vault_signer_nonce,
        )?;

        let user_token_lp = Self::ata_for(&owner, &state.lp_mint, &TOKEN_PROGRAM_ID);
        let user_token_coin = Self::ata_for(&owner, &state.base_mint, &TOKEN_PROGRAM_ID);
        let user_token_pc = Self::ata_for(&owner, &state.quote_mint, &TOKEN_PROGRAM_ID);

        let mut instructions = Vec::new();
        instructions.push(create_associated_token_account_idempotent(
            &owner,
            &owner,
            &state.base_mint,
            &TOKEN_PROGRAM_ID,
        ));
        instructions.push(create_associated_token_account_idempotent(
            &owner,
            &owner,
            &state.quote_mint,
            &TOKEN_PROGRAM_ID,
        ));

        let mut data = Vec::with_capacity(1 + 8 + 8 + 8);
        data.push(4);
        data.extend_from_slice(&effective_lp_amount.to_le_bytes());
        data.extend_from_slice(&min_base_amount_out.to_le_bytes());
        data.extend_from_slice(&min_quote_amount_out.to_le_bytes());

        instructions.push(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new(*pool, false),
                AccountMeta::new_readonly(authority, false),
                AccountMeta::new(state.open_orders, false),
                AccountMeta::new(state.target_orders, false),
                AccountMeta::new(state.lp_mint, false),
                AccountMeta::new(state.base_vault, false),
                AccountMeta::new(state.quote_vault, false),
                AccountMeta::new_readonly(state.market_program_id, false),
                AccountMeta::new(state.market_id, false),
                AccountMeta::new(market_state.base_vault, false),
                AccountMeta::new(market_state.quote_vault, false),
                AccountMeta::new_readonly(market_vault_signer, false),
                AccountMeta::new(user_token_lp, false),
                AccountMeta::new(user_token_coin, false),
                AccountMeta::new(user_token_pc, false),
                AccountMeta::new_readonly(owner, true),
                AccountMeta::new(market_state.event_queue, false),
                AccountMeta::new(market_state.bids, false),
                AccountMeta::new(market_state.asks, false),
            ],
            data,
        });

        Ok((
            instructions,
            state,
            effective_lp_amount,
            base_out,
            quote_out,
        ))
    }

    async fn build_swap_accounts(
        &self,
        pool: &Pubkey,
        state: &RaydiumAmmV4PoolState,
        user_source: Pubkey,
        user_destination: Pubkey,
        user_owner: Pubkey,
    ) -> anyhow::Result<(Vec<AccountMeta>, bool)> {
        let authority = Self::derive_amm_authority_pda(state.nonce)?;
        if Self::status_supports_orderbook(state.status) {
            anyhow::ensure!(
                state.market_program_id != Pubkey::default(),
                "raydium amm v4 market program id is unset"
            );

            let market_state = self.fetch_market_state(&state.market_id).await?;
            anyhow::ensure!(
                market_state.base_mint == state.base_mint
                    && market_state.quote_mint == state.quote_mint,
                "raydium amm v4 market mints do not match pool mints"
            );

            let market_vault_signer = Self::derive_market_vault_signer(
                &state.market_id,
                &state.market_program_id,
                market_state.vault_signer_nonce,
            )?;

            let accounts = vec![
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new(*pool, false),
                AccountMeta::new_readonly(authority, false),
                AccountMeta::new(state.open_orders, false),
                AccountMeta::new(state.target_orders, false),
                AccountMeta::new(state.base_vault, false),
                AccountMeta::new(state.quote_vault, false),
                AccountMeta::new_readonly(state.market_program_id, false),
                AccountMeta::new(state.market_id, false),
                AccountMeta::new(market_state.bids, false),
                AccountMeta::new(market_state.asks, false),
                AccountMeta::new(market_state.event_queue, false),
                AccountMeta::new(market_state.base_vault, false),
                AccountMeta::new(market_state.quote_vault, false),
                AccountMeta::new_readonly(market_vault_signer, false),
                AccountMeta::new(user_source, false),
                AccountMeta::new(user_destination, false),
                AccountMeta::new_readonly(user_owner, true),
            ];
            return Ok((accounts, true));
        }

        let accounts = vec![
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(state.base_vault, false),
            AccountMeta::new(state.quote_vault, false),
            AccountMeta::new(user_source, false),
            AccountMeta::new(user_destination, false),
            AccountMeta::new_readonly(user_owner, true),
        ];

        Ok((accounts, false))
    }

    pub async fn buy(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.buy_with_priority_fee_override(
            mint,
            pool,
            _creator,
            sol_amount_in,
            slippage,
            price,
            use_idempotent,
            None,
        )
        .await
    }

    pub async fn buy_with_priority_fee_override(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        let buyer = self.keypair.pubkey();
        self.buy_for_user_with_priority_fee_override(
            &buyer,
            mint,
            pool,
            _creator,
            sol_amount_in,
            slippage,
            price,
            use_idempotent,
            priority_fee_override,
        )
        .await
    }

    pub async fn buy_for_user(
        &self,
        buyer: &Pubkey,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.buy_for_user_with_priority_fee_override(
            buyer,
            mint,
            pool,
            _creator,
            sol_amount_in,
            slippage,
            price,
            use_idempotent,
            None,
        )
        .await
    }

    pub async fn buy_for_user_with_priority_fee_override(
        &self,
        buyer: &Pubkey,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        anyhow::ensure!(price > 0.0, "raydium amm v4 buy price must be > 0");
        anyhow::ensure!(sol_amount_in > 0.0, "raydium amm v4 buy amount must be > 0");
        anyhow::ensure!(
            *mint != WSOL_MINT,
            "raydium amm v4 buy mint must not be WSOL"
        );

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch raydium amm v4 state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;
        Self::validate_swap_enabled(&state)?;

        let output_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .context("failed to resolve output token program for raydium amm v4 buy")?;
        anyhow::ensure!(
            output_program == TOKEN_PROGRAM_ID,
            "raydium amm v4 only supports token-program mints for swaps"
        );

        let input_program = TOKEN_PROGRAM_ID;
        let input_ata = Self::ata_for(&buyer, &WSOL_MINT, &input_program);
        let output_ata = Self::ata_for(&buyer, mint, &output_program);

        let use_idempotent = use_idempotent.unwrap_or(false);
        let mut ixs = Vec::new();
        if use_idempotent {
            ixs.push(create_associated_token_account_idempotent(
                &buyer,
                &buyer,
                mint,
                &output_program,
            ));
            ixs.push(create_associated_token_account_idempotent(
                &buyer,
                &buyer,
                &WSOL_MINT,
                &input_program,
            ));
        } else {
            ixs.push(create_associated_token_account(
                &buyer,
                &buyer,
                mint,
                &output_program,
            ));
            ixs.push(create_associated_token_account(
                &buyer,
                &buyer,
                &WSOL_MINT,
                &input_program,
            ));
        }

        let slippage_pct = Self::normalize_slippage(slippage);
        let amount_in = (sol_amount_in * 1e9).round() as u64;
        anyhow::ensure!(amount_in > 0, "raydium amm v4 buy amount is too small");

        let output_decimals = Self::token_decimals_for_mint(&state, mint)?;
        let min_amount_out =
            Self::quote_buy_min_output_raw(sol_amount_in, price, slippage_pct, output_decimals);

        ixs.push(system_instruction_if::transfer(
            &buyer, &input_ata, amount_in,
        ));
        ixs.push(sync_native(&input_program, &input_ata)?);

        let (mut accounts, use_orderbook_accounts) = self
            .build_swap_accounts(pool, &state, input_ata, output_ata, buyer)
            .await?;

        let recent_fees = self
            .sol
            .resolve_priority_fee(
                priority_fee_override,
                &accounts
                    .iter()
                    .map(|account| account.pubkey)
                    .collect::<Vec<Pubkey>>(),
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            )
            .await
            .context("failed to resolve priority fee for raydium amm v4 buy")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_base_in_instruction_data(
            amount_in,
            min_amount_out,
            use_orderbook_accounts,
        );
        ixs.push(Instruction {
            program_id: RAYDIUM_AMM_V4_ID,
            accounts: std::mem::take(&mut accounts),
            data,
        });

        Ok((ixs, recent_fees))
    }

    pub async fn sell(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        price: f64,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.sell_with_priority_fee_override(mint, pool, _creator, sell_pct, slippage, price, None)
            .await
    }

    pub async fn sell_with_priority_fee_override(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        price: f64,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        let buyer = self.keypair.pubkey();
        self.sell_for_user_with_priority_fee_override(
            &buyer,
            mint,
            pool,
            _creator,
            sell_pct,
            slippage,
            price,
            priority_fee_override,
        )
        .await
    }

    pub async fn sell_for_user(
        &self,
        buyer: &Pubkey,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        price: f64,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.sell_for_user_with_priority_fee_override(
            buyer, mint, pool, _creator, sell_pct, slippage, price, None,
        )
        .await
    }

    pub async fn sell_for_user_with_priority_fee_override(
        &self,
        buyer: &Pubkey,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        price: f64,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        anyhow::ensure!(price > 0.0, "raydium amm v4 sell price must be > 0");
        anyhow::ensure!(
            *mint != WSOL_MINT,
            "raydium amm v4 sell mint must not be WSOL"
        );

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch raydium amm v4 state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;
        Self::validate_swap_enabled(&state)?;

        let sell_pct = sell_pct.clamp(1, 100);

        let input_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .context("failed to resolve input token program for raydium amm v4 sell")?;
        anyhow::ensure!(
            input_program == TOKEN_PROGRAM_ID,
            "raydium amm v4 only supports token-program mints for swaps"
        );

        let input_ata = Self::ata_for(&buyer, mint, &input_program);
        let output_program = TOKEN_PROGRAM_ID;
        let output_ata = Self::ata_for(&buyer, &WSOL_MINT, &output_program);

        let token_balance_raw = self
            .user_token_balance_raw(&buyer, mint)
            .await
            .context("failed to fetch token balance for raydium amm v4 sell")?;
        anyhow::ensure!(
            token_balance_raw > 0,
            "no token balance for raydium amm v4 sell"
        );

        let amount_in = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            amount_in > 0,
            "raydium amm v4 sell amount is too small for requested percentage"
        );

        let slippage_pct = Self::normalize_slippage(slippage);
        let input_decimals = Self::token_decimals_for_mint(&state, mint)?;
        let min_sol_output =
            Self::quote_sell_min_output_raw(amount_in, input_decimals, price, slippage_pct);

        let mut ixs = vec![create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &output_program,
        )];

        let (mut accounts, use_orderbook_accounts) = self
            .build_swap_accounts(pool, &state, input_ata, output_ata, buyer)
            .await?;

        let recent_fees = self
            .sol
            .resolve_priority_fee(
                priority_fee_override,
                &accounts
                    .iter()
                    .map(|account| account.pubkey)
                    .collect::<Vec<Pubkey>>(),
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            )
            .await
            .context("failed to resolve priority fee for raydium amm v4 sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_base_in_instruction_data(
            amount_in,
            min_sol_output,
            use_orderbook_accounts,
        );
        ixs.push(Instruction {
            program_id: RAYDIUM_AMM_V4_ID,
            accounts: std::mem::take(&mut accounts),
            data,
        });

        if amount_in == token_balance_raw && self.account_exists(&input_ata).await? {
            let close_input_ix =
                self.sol
                    .close_token_account_ix(&input_program, &input_ata, &buyer, &buyer)?;
            ixs.push(close_input_ix);
        }
        if amount_in == token_balance_raw && self.account_exists(&output_ata).await? {
            let close_wsol_ix =
                self.sol
                    .close_token_account_ix(&output_program, &output_ata, &buyer, &buyer)?;
            ixs.push(close_wsol_ix);
        }

        Ok((ixs, recent_fees))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    use serde_json::json;

    fn encode_fixture_event<T: Serialize>(event: &T) -> String {
        let bytes = bincode::serialize(event).unwrap();
        B64.encode(bytes)
    }

    fn synthetic_pool_state_account_bytes() -> Vec<u8> {
        let mut data = vec![0u8; AMM_V4_POOL_ACCOUNT_LEN];

        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_mint = Pubkey::new_unique();
        let lp_mint = Pubkey::new_unique();
        let open_orders = Pubkey::new_unique();
        let market_id = Pubkey::new_unique();
        let target_orders = Pubkey::new_unique();
        let withdraw_queue = Pubkey::new_unique();
        let lp_vault = Pubkey::new_unique();
        let owner = Pubkey::new_unique();

        data[AMM_V4_POOL_STATUS_OFFSET..AMM_V4_POOL_STATUS_OFFSET + 8]
            .copy_from_slice(&AMM_STATUS_SWAP_ONLY.to_le_bytes());
        data[AMM_V4_POOL_NONCE_OFFSET..AMM_V4_POOL_NONCE_OFFSET + 8]
            .copy_from_slice(&250u64.to_le_bytes());
        data[AMM_V4_POOL_BASE_DECIMALS_OFFSET..AMM_V4_POOL_BASE_DECIMALS_OFFSET + 8]
            .copy_from_slice(&6u64.to_le_bytes());
        data[AMM_V4_POOL_QUOTE_DECIMALS_OFFSET..AMM_V4_POOL_QUOTE_DECIMALS_OFFSET + 8]
            .copy_from_slice(&9u64.to_le_bytes());
        data[AMM_V4_POOL_STATE_OFFSET..AMM_V4_POOL_STATE_OFFSET + 8]
            .copy_from_slice(&1u64.to_le_bytes());
        data[AMM_V4_POOL_RESET_FLAG_OFFSET..AMM_V4_POOL_RESET_FLAG_OFFSET + 8]
            .copy_from_slice(&0u64.to_le_bytes());
        data[AMM_V4_POOL_TRADE_FEE_NUMERATOR_OFFSET..AMM_V4_POOL_TRADE_FEE_NUMERATOR_OFFSET + 8]
            .copy_from_slice(&25u64.to_le_bytes());
        data[AMM_V4_POOL_TRADE_FEE_DENOMINATOR_OFFSET
            ..AMM_V4_POOL_TRADE_FEE_DENOMINATOR_OFFSET + 8]
            .copy_from_slice(&10_000u64.to_le_bytes());
        data[AMM_V4_POOL_SWAP_FEE_NUMERATOR_OFFSET..AMM_V4_POOL_SWAP_FEE_NUMERATOR_OFFSET + 8]
            .copy_from_slice(&25u64.to_le_bytes());
        data[AMM_V4_POOL_SWAP_FEE_DENOMINATOR_OFFSET..AMM_V4_POOL_SWAP_FEE_DENOMINATOR_OFFSET + 8]
            .copy_from_slice(&10_000u64.to_le_bytes());
        data[AMM_V4_POOL_BASE_NEED_TAKE_PNL_OFFSET..AMM_V4_POOL_BASE_NEED_TAKE_PNL_OFFSET + 8]
            .copy_from_slice(&10u64.to_le_bytes());
        data[AMM_V4_POOL_QUOTE_NEED_TAKE_PNL_OFFSET..AMM_V4_POOL_QUOTE_NEED_TAKE_PNL_OFFSET + 8]
            .copy_from_slice(&20u64.to_le_bytes());
        data[AMM_V4_POOL_POOL_OPEN_TIME_OFFSET..AMM_V4_POOL_POOL_OPEN_TIME_OFFSET + 8]
            .copy_from_slice(&1_700_000_000u64.to_le_bytes());
        data[AMM_V4_POOL_ORDERBOOK_TO_INIT_TIME_OFFSET
            ..AMM_V4_POOL_ORDERBOOK_TO_INIT_TIME_OFFSET + 8]
            .copy_from_slice(&1_700_000_100u64.to_le_bytes());

        data[AMM_V4_POOL_BASE_VAULT_OFFSET..AMM_V4_POOL_BASE_VAULT_OFFSET + 32]
            .copy_from_slice(base_vault.as_ref());
        data[AMM_V4_POOL_QUOTE_VAULT_OFFSET..AMM_V4_POOL_QUOTE_VAULT_OFFSET + 32]
            .copy_from_slice(quote_vault.as_ref());
        data[AMM_V4_POOL_BASE_MINT_OFFSET..AMM_V4_POOL_BASE_MINT_OFFSET + 32]
            .copy_from_slice(base_mint.as_ref());
        data[AMM_V4_POOL_QUOTE_MINT_OFFSET..AMM_V4_POOL_QUOTE_MINT_OFFSET + 32]
            .copy_from_slice(WSOL_MINT.as_ref());
        data[AMM_V4_POOL_LP_MINT_OFFSET..AMM_V4_POOL_LP_MINT_OFFSET + 32]
            .copy_from_slice(lp_mint.as_ref());
        data[AMM_V4_POOL_OPEN_ORDERS_OFFSET..AMM_V4_POOL_OPEN_ORDERS_OFFSET + 32]
            .copy_from_slice(open_orders.as_ref());
        data[AMM_V4_POOL_MARKET_ID_OFFSET..AMM_V4_POOL_MARKET_ID_OFFSET + 32]
            .copy_from_slice(market_id.as_ref());
        data[AMM_V4_POOL_MARKET_PROGRAM_ID_OFFSET..AMM_V4_POOL_MARKET_PROGRAM_ID_OFFSET + 32]
            .copy_from_slice(Pubkey::new_unique().as_ref());
        data[AMM_V4_POOL_TARGET_ORDERS_OFFSET..AMM_V4_POOL_TARGET_ORDERS_OFFSET + 32]
            .copy_from_slice(target_orders.as_ref());
        data[AMM_V4_POOL_WITHDRAW_QUEUE_OFFSET..AMM_V4_POOL_WITHDRAW_QUEUE_OFFSET + 32]
            .copy_from_slice(withdraw_queue.as_ref());
        data[AMM_V4_POOL_LP_VAULT_OFFSET..AMM_V4_POOL_LP_VAULT_OFFSET + 32]
            .copy_from_slice(lp_vault.as_ref());
        data[AMM_V4_POOL_OWNER_OFFSET..AMM_V4_POOL_OWNER_OFFSET + 32]
            .copy_from_slice(owner.as_ref());
        data[AMM_V4_POOL_LP_RESERVE_OFFSET..AMM_V4_POOL_LP_RESERVE_OFFSET + 8]
            .copy_from_slice(&9_999_999u64.to_le_bytes());

        data
    }

    fn synthetic_market_state_account_bytes(base_mint: Pubkey, quote_mint: Pubkey) -> Vec<u8> {
        let mut data = vec![0u8; OPENBOOK_V3_MARKET_MIN_LEN];

        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let event_queue = Pubkey::new_unique();
        let bids = Pubkey::new_unique();
        let asks = Pubkey::new_unique();

        data[OPENBOOK_V3_MARKET_VAULT_SIGNER_NONCE_OFFSET
            ..OPENBOOK_V3_MARKET_VAULT_SIGNER_NONCE_OFFSET + 8]
            .copy_from_slice(&42u64.to_le_bytes());
        data[OPENBOOK_V3_MARKET_BASE_MINT_OFFSET..OPENBOOK_V3_MARKET_BASE_MINT_OFFSET + 32]
            .copy_from_slice(base_mint.as_ref());
        data[OPENBOOK_V3_MARKET_QUOTE_MINT_OFFSET..OPENBOOK_V3_MARKET_QUOTE_MINT_OFFSET + 32]
            .copy_from_slice(quote_mint.as_ref());
        data[OPENBOOK_V3_MARKET_BASE_VAULT_OFFSET..OPENBOOK_V3_MARKET_BASE_VAULT_OFFSET + 32]
            .copy_from_slice(base_vault.as_ref());
        data[OPENBOOK_V3_MARKET_QUOTE_VAULT_OFFSET..OPENBOOK_V3_MARKET_QUOTE_VAULT_OFFSET + 32]
            .copy_from_slice(quote_vault.as_ref());
        data[OPENBOOK_V3_MARKET_EVENT_QUEUE_OFFSET..OPENBOOK_V3_MARKET_EVENT_QUEUE_OFFSET + 32]
            .copy_from_slice(event_queue.as_ref());
        data[OPENBOOK_V3_MARKET_BIDS_OFFSET..OPENBOOK_V3_MARKET_BIDS_OFFSET + 32]
            .copy_from_slice(bids.as_ref());
        data[OPENBOOK_V3_MARKET_ASKS_OFFSET..OPENBOOK_V3_MARKET_ASKS_OFFSET + 32]
            .copy_from_slice(asks.as_ref());

        data
    }

    #[test]
    fn test_raydium_amm_v4_program_constants() {
        assert_eq!(
            RAYDIUM_AMM_V4_ID,
            Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap()
        );
        assert_eq!(AMM_V4_POOL_ACCOUNT_LEN, 752);
        assert_eq!(AMM_V4_POOL_BASE_MINT_OFFSET, 400);
        assert_eq!(AMM_V4_POOL_QUOTE_MINT_OFFSET, 432);
    }

    #[test]
    fn test_raydium_amm_v4_instruction_tags() {
        assert_eq!(SWAP_BASE_IN_IX_TAG, 9);
        assert_eq!(SWAP_BASE_IN_V2_IX_TAG, 16);
    }

    #[test]
    fn test_raydium_amm_v4_normalize_slippage() {
        assert_eq!(RaydiumAmmV4::normalize_slippage(15.0), 0.15);
        assert_eq!(RaydiumAmmV4::normalize_slippage(0.2), 0.2);
        assert_eq!(RaydiumAmmV4::normalize_slippage(0.0), 0.01);
        assert_eq!(RaydiumAmmV4::normalize_slippage(120.0), 0.99);
    }

    #[test]
    fn test_raydium_amm_v4_encode_swap_instruction_data_layout() {
        let data_full = RaydiumAmmV4::encode_swap_base_in_instruction_data(1_234, 9_876, true);
        assert_eq!(data_full.len(), 17);
        assert_eq!(data_full[0], SWAP_BASE_IN_IX_TAG);
        assert_eq!(
            u64::from_le_bytes(data_full[1..9].try_into().unwrap()),
            1_234
        );
        assert_eq!(
            u64::from_le_bytes(data_full[9..17].try_into().unwrap()),
            9_876
        );

        let data_v2 = RaydiumAmmV4::encode_swap_base_in_instruction_data(50, 99, false);
        assert_eq!(data_v2[0], SWAP_BASE_IN_V2_IX_TAG);
    }

    #[test]
    fn test_raydium_amm_v4_decode_pool_state_layout() {
        let data = synthetic_pool_state_account_bytes();
        let state = RaydiumAmmV4::decode_pool_state_account_data(&data).unwrap();

        assert_eq!(state.status, AMM_STATUS_SWAP_ONLY);
        assert_eq!(state.base_decimals, 6);
        assert_eq!(state.quote_decimals, 9);
        assert_eq!(state.quote_mint, WSOL_MINT);
        assert_eq!(state.base_need_take_pnl, 10);
        assert_eq!(state.quote_need_take_pnl, 20);
        assert_eq!(state.lp_reserve, 9_999_999);
    }

    #[test]
    fn test_raydium_amm_v4_decode_market_state_layout() {
        let base_mint = Pubkey::new_unique();
        let data = synthetic_market_state_account_bytes(base_mint, WSOL_MINT);
        let state = RaydiumAmmV4::decode_market_state_account_data(&data).unwrap();

        assert_eq!(state.vault_signer_nonce, 42);
        assert_eq!(state.base_mint, base_mint);
        assert_eq!(state.quote_mint, WSOL_MINT);
        assert_ne!(state.base_vault, Pubkey::default());
        assert_ne!(state.quote_vault, Pubkey::default());
        assert_ne!(state.bids, Pubkey::default());
        assert_ne!(state.asks, Pubkey::default());
        assert_ne!(state.event_queue, Pubkey::default());
    }

    #[test]
    fn test_raydium_amm_v4_sol_price_from_vault_amounts_uses_net_vaults() {
        let state =
            RaydiumAmmV4::decode_pool_state_account_data(&synthetic_pool_state_account_bytes())
                .unwrap();
        let price =
            RaydiumAmmV4::sol_price_from_vault_amounts(&state, 200_000_000, 1_000_000_000).unwrap();
        assert!((price - 0.00500000015).abs() < 1e-9);
    }

    #[test]
    fn test_raydium_amm_v4_parse_logs_ignores_invalid_payloads() {
        let logs = vec![
            "Program log: hello".to_string(),
            "Program log: ray_log: not-base64".to_string(),
            format!("Program log: ray_log: {}", B64.encode([1u8, 2, 3, 4])),
        ];
        let events = RaydiumAmmV4::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_raydium_amm_v4_parse_logs_unknown_event() {
        let payload = [9u8, 1, 1, 1, 1, 1, 1, 1];
        let logs = vec![format!("Program log: ray_log: {}", B64.encode(payload))];
        let events = RaydiumAmmV4::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], RaydiumAmmV4Event::Unknown));
    }

    #[test]
    fn test_raydium_amm_v4_parse_logs_swap_base_in_fixture_decodes() {
        let fixture = SwapBaseInRayLog {
            log_type: RAY_LOG_TYPE_SWAP_BASE_IN,
            amount_in: 1_000,
            minimum_out: 900,
            direction: 2,
            user_source: 1_234,
            pool_coin: 50_000,
            pool_pc: 60_000,
            out_amount: 990,
        };
        let logs = vec![format!(
            "Program log: ray_log: {}",
            encode_fixture_event(&fixture)
        )];

        let events = RaydiumAmmV4::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumAmmV4Event::SwapBaseIn(Some(event)) => {
                assert_eq!(event.amount_in, fixture.amount_in);
                assert_eq!(event.minimum_out, fixture.minimum_out);
                assert_eq!(event.direction, fixture.direction);
                assert_eq!(event.out_amount, fixture.out_amount);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_raydium_amm_v4_parse_logs_swap_base_out_fixture_decodes() {
        let fixture = SwapBaseOutRayLog {
            log_type: RAY_LOG_TYPE_SWAP_BASE_OUT,
            max_in: 1_500,
            amount_out: 1_000,
            direction: 1,
            user_source: 555,
            pool_coin: 44_000,
            pool_pc: 55_000,
            deduct_in: 1_123,
        };
        let logs = vec![format!(
            "Program log: ray_log: {}",
            encode_fixture_event(&fixture)
        )];

        let events = RaydiumAmmV4::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumAmmV4Event::SwapBaseOut(Some(event)) => {
                assert_eq!(event.max_in, fixture.max_in);
                assert_eq!(event.amount_out, fixture.amount_out);
                assert_eq!(event.direction, fixture.direction);
                assert_eq!(event.deduct_in, fixture.deduct_in);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_raydium_amm_v4_extract_pool_from_inner_instruction_fixture() {
        let token_program = TOKEN_PROGRAM_ID;
        let pool = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let open_orders = Pubkey::new_unique();
        let target_orders = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let market_program = Pubkey::new_unique();
        let market = Pubkey::new_unique();
        let bids = Pubkey::new_unique();
        let asks = Pubkey::new_unique();
        let event_queue = Pubkey::new_unique();
        let market_base_vault = Pubkey::new_unique();
        let market_quote_vault = Pubkey::new_unique();
        let market_vault_signer = Pubkey::new_unique();
        let user_source = Pubkey::new_unique();
        let user_destination = Pubkey::new_unique();
        let user_owner = Pubkey::new_unique();

        let tx_json = json!({
            "slot": 1,
            "transaction": {
                "signatures": ["1111111111111111111111111111111111111111111111111111111111111111"],
                "message": {
                    "accountKeys": [
                        {
                            "pubkey": Pubkey::new_unique().to_string(),
                            "writable": false,
                            "signer": false
                        }
                    ],
                    "recentBlockhash": "11111111111111111111111111111111",
                    "instructions": [
                        {
                            "program": "jupiter",
                            "programId": "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4",
                            "parsed": {
                                "type": "route",
                                "info": {}
                            }
                        }
                    ]
                }
            },
            "meta": {
                "err": null,
                "status": {
                    "Ok": null
                },
                "fee": 5000,
                "preBalances": [0],
                "postBalances": [0],
                "innerInstructions": [
                    {
                        "index": 0,
                        "instructions": [
                            {
                                "programId": RAYDIUM_AMM_V4_ID.to_string(),
                                "accounts": [
                                    token_program.to_string(),
                                    pool.to_string(),
                                    authority.to_string(),
                                    open_orders.to_string(),
                                    target_orders.to_string(),
                                    base_vault.to_string(),
                                    quote_vault.to_string(),
                                    market_program.to_string(),
                                    market.to_string(),
                                    bids.to_string(),
                                    asks.to_string(),
                                    event_queue.to_string(),
                                    market_base_vault.to_string(),
                                    market_quote_vault.to_string(),
                                    market_vault_signer.to_string(),
                                    user_source.to_string(),
                                    user_destination.to_string(),
                                    user_owner.to_string()
                                ],
                                "data": B64.encode(vec![9u8; 17]),
                                "stackHeight": 2
                            }
                        ]
                    }
                ]
            }
        });

        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(tx_json).unwrap();
        assert_eq!(RaydiumAmmV4::extract_pool_from_transaction(&tx), Some(pool));
    }
}
