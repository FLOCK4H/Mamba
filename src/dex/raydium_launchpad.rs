use crate::core::cluster::{
    RAYDIUM_LAUNCHPAD_AUTHORITY_PDA_DEVNET, RAYDIUM_LAUNCHPAD_AUTHORITY_PDA_MAINNET,
    RAYDIUM_LAUNCHPAD_PROGRAM_ID_DEVNET, RAYDIUM_LAUNCHPAD_PROGRAM_ID_MAINNET,
};
use crate::core::sol::{
    DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SYSTEM_PROGRAM, SolHook,
    TOKEN_PROGRAM_ID, WSOL_MINT,
};
use crate::utils::utils::decode_b64;
use crate::utils::writing::cc;
use crate::{log, warn};
use anyhow::Context;
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
use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

pub const RAYDIUM_LAUNCHPAD_ID: Pubkey = RAYDIUM_LAUNCHPAD_PROGRAM_ID_MAINNET;
pub const RAYDIUM_LAUNCHPAD_AUTH: Pubkey = RAYDIUM_LAUNCHPAD_AUTHORITY_PDA_MAINNET;

pub const RAYDIUM_LAUNCHPAD_DEVNET_ID: Pubkey = RAYDIUM_LAUNCHPAD_PROGRAM_ID_DEVNET;
pub const RAYDIUM_LAUNCHPAD_DEVNET_AUTH: Pubkey = RAYDIUM_LAUNCHPAD_AUTHORITY_PDA_DEVNET;

pub const LAUNCHPAD_AUTH_SEED: &[u8] = b"vault_auth_seed";
pub const LAUNCHPAD_EVENT_AUTH_SEED: &[u8] = b"__event_authority";

pub const BUY_EXACT_IN_IX_DISCRIM: [u8; 8] = [250, 234, 13, 123, 213, 156, 19, 236];
pub const BUY_EXACT_OUT_IX_DISCRIM: [u8; 8] = [24, 211, 116, 40, 105, 3, 153, 56];
pub const SELL_EXACT_IN_IX_DISCRIM: [u8; 8] = [149, 39, 222, 155, 211, 124, 152, 26];
pub const SELL_EXACT_OUT_IX_DISCRIM: [u8; 8] = [95, 200, 71, 34, 8, 9, 11, 166];

pub const GLOBAL_CONFIG_DISCRIM: [u8; 8] = [149, 8, 156, 202, 160, 252, 176, 217];
pub const PLATFORM_CONFIG_DISCRIM: [u8; 8] = [160, 78, 128, 0, 248, 83, 230, 160];
pub const POOL_STATE_DISCRIM: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];

pub const POOL_CREATE_EVENT_DISCRIM: [u8; 8] = [151, 215, 226, 9, 118, 161, 115, 174];
pub const TRADE_EVENT_DISCRIM: [u8; 8] = [189, 219, 127, 211, 78, 230, 97, 238];

pub const SEARCH_FOR_PROGRAM_DATA: &str = "Program data: ";

pub const LAUNCHPAD_POOL_ACCOUNT_LEN: usize = 429;
pub const LAUNCHPAD_GLOBAL_CONFIG_ACCOUNT_LEN: usize = 371;
pub const LAUNCHPAD_PLATFORM_CONFIG_ACCOUNT_MIN_LEN: usize = 944;

pub const LAUNCHPAD_POOL_EPOCH_OFFSET: usize = 8;
pub const LAUNCHPAD_POOL_AUTH_BUMP_OFFSET: usize = 16;
pub const LAUNCHPAD_POOL_STATUS_OFFSET: usize = 17;
pub const LAUNCHPAD_POOL_BASE_DECIMALS_OFFSET: usize = 18;
pub const LAUNCHPAD_POOL_QUOTE_DECIMALS_OFFSET: usize = 19;
pub const LAUNCHPAD_POOL_MIGRATE_TYPE_OFFSET: usize = 20;
pub const LAUNCHPAD_POOL_SUPPLY_OFFSET: usize = 21;
pub const LAUNCHPAD_POOL_TOTAL_BASE_SELL_OFFSET: usize = 29;
pub const LAUNCHPAD_POOL_VIRTUAL_BASE_OFFSET: usize = 37;
pub const LAUNCHPAD_POOL_VIRTUAL_QUOTE_OFFSET: usize = 45;
pub const LAUNCHPAD_POOL_REAL_BASE_OFFSET: usize = 53;
pub const LAUNCHPAD_POOL_REAL_QUOTE_OFFSET: usize = 61;
pub const LAUNCHPAD_POOL_TOTAL_QUOTE_FUND_RAISING_OFFSET: usize = 69;
pub const LAUNCHPAD_POOL_QUOTE_PROTOCOL_FEE_OFFSET: usize = 77;
pub const LAUNCHPAD_POOL_PLATFORM_FEE_OFFSET: usize = 85;
pub const LAUNCHPAD_POOL_MIGRATE_FEE_OFFSET: usize = 93;
pub const LAUNCHPAD_POOL_VESTING_TOTAL_LOCKED_AMOUNT_OFFSET: usize = 101;
pub const LAUNCHPAD_POOL_VESTING_CLIFF_PERIOD_OFFSET: usize = 109;
pub const LAUNCHPAD_POOL_VESTING_UNLOCK_PERIOD_OFFSET: usize = 117;
pub const LAUNCHPAD_POOL_VESTING_START_TIME_OFFSET: usize = 125;
pub const LAUNCHPAD_POOL_VESTING_ALLOCATED_SHARE_AMOUNT_OFFSET: usize = 133;
pub const LAUNCHPAD_POOL_GLOBAL_CONFIG_OFFSET: usize = 141;
pub const LAUNCHPAD_POOL_PLATFORM_CONFIG_OFFSET: usize = 173;
pub const LAUNCHPAD_POOL_BASE_MINT_OFFSET: usize = 205;
pub const LAUNCHPAD_POOL_QUOTE_MINT_OFFSET: usize = 237;
pub const LAUNCHPAD_POOL_BASE_VAULT_OFFSET: usize = 269;
pub const LAUNCHPAD_POOL_QUOTE_VAULT_OFFSET: usize = 301;
pub const LAUNCHPAD_POOL_CREATOR_OFFSET: usize = 333;
pub const LAUNCHPAD_POOL_TOKEN_PROGRAM_FLAG_OFFSET: usize = 365;
pub const LAUNCHPAD_POOL_AMM_FEE_ON_OFFSET: usize = 366;

pub const LAUNCHPAD_GLOBAL_CONFIG_CURVE_TYPE_OFFSET: usize = 16;
pub const LAUNCHPAD_GLOBAL_CONFIG_TRADE_FEE_RATE_OFFSET: usize = 27;
pub const LAUNCHPAD_GLOBAL_CONFIG_MAX_SHARE_FEE_RATE_OFFSET: usize = 35;
pub const LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_SUPPLY_OFFSET: usize = 43;
pub const LAUNCHPAD_GLOBAL_CONFIG_MAX_LOCK_RATE_OFFSET: usize = 51;
pub const LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_SELL_RATE_OFFSET: usize = 59;
pub const LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_MIGRATE_RATE_OFFSET: usize = 67;
pub const LAUNCHPAD_GLOBAL_CONFIG_MIN_QUOTE_FUND_RAISING_OFFSET: usize = 75;
pub const LAUNCHPAD_GLOBAL_CONFIG_QUOTE_MINT_OFFSET: usize = 83;

pub const LAUNCHPAD_PLATFORM_CONFIG_PLATFORM_FEE_WALLET_OFFSET: usize = 16;
pub const LAUNCHPAD_PLATFORM_CONFIG_FEE_RATE_OFFSET: usize = 104;
pub const LAUNCHPAD_PLATFORM_CONFIG_NAME_OFFSET: usize = 112;
pub const LAUNCHPAD_PLATFORM_CONFIG_NAME_LEN: usize = 64;
pub const LAUNCHPAD_PLATFORM_CONFIG_WEB_OFFSET: usize = 176;
pub const LAUNCHPAD_PLATFORM_CONFIG_WEB_LEN: usize = 256;
pub const LAUNCHPAD_PLATFORM_CONFIG_IMG_OFFSET: usize = 432;
pub const LAUNCHPAD_PLATFORM_CONFIG_IMG_LEN: usize = 256;
pub const LAUNCHPAD_PLATFORM_CONFIG_CREATOR_FEE_RATE_OFFSET: usize = 720;
pub const LAUNCHPAD_PLATFORM_CONFIG_CURVE_PARAMS_OFFSET: usize = 940;

const FEE_RATE_DENOMINATOR: u64 = 1_000_000;
const LINEAR_Q64: u128 = 1u128 << 64;
const DEFAULT_SHARE_FEE_RATE: u64 = 0;

const LAUNCHPAD_POOL_STATUS_FUND: u8 = 0;
const LAUNCHPAD_POOL_STATUS_MIGRATE: u8 = 1;
const LAUNCHPAD_POOL_STATUS_TRADE: u8 = 2;

pub const LAUNCHPAD_TRADE_DIRECTION_BUY: u8 = 0;
pub const LAUNCHPAD_TRADE_DIRECTION_SELL: u8 = 1;

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadVestingSchedule {
    pub total_locked_amount: u64,
    pub cliff_period: u64,
    pub unlock_period: u64,
    pub start_time: u64,
    pub allocated_share_amount: u64,
}

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadPoolState {
    pub epoch: u64,
    pub auth_bump: u8,
    pub status: u8,
    pub base_decimals: u8,
    pub quote_decimals: u8,
    pub migrate_type: u8,
    pub supply: u64,
    pub total_base_sell: u64,
    pub virtual_base: u64,
    pub virtual_quote: u64,
    pub real_base: u64,
    pub real_quote: u64,
    pub total_quote_fund_raising: u64,
    pub quote_protocol_fee: u64,
    pub platform_fee: u64,
    pub migrate_fee: u64,
    pub vesting_schedule: RaydiumLaunchpadVestingSchedule,
    pub global_config: Pubkey,
    pub platform_config: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub creator: Pubkey,
    pub token_program_flag: u8,
    pub amm_fee_on: u8,
}

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadGlobalConfigState {
    pub curve_type: u8,
    pub trade_fee_rate: u64,
    pub max_share_fee_rate: u64,
    pub min_base_supply: u64,
    pub max_lock_rate: u64,
    pub min_base_sell_rate: u64,
    pub min_base_migrate_rate: u64,
    pub min_quote_fund_raising: u64,
    pub quote_mint: Pubkey,
}

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadPlatformConfigState {
    pub platform_fee_wallet: Pubkey,
    pub fee_rate: u64,
    pub creator_fee_rate: u64,
}

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadPlatformConfigInfo {
    pub name: String,
    pub web: String,
    pub img: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaydiumLaunchpadBondingCurveParam {
    pub migrate_type: u8,
    pub amm_fee_on: u8,
    pub supply: u64,
    pub total_base_sell: u64,
    pub total_quote_fund_raising: u64,
    pub total_locked_amount: u64,
    pub cliff_period: u64,
    pub unlock_period: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaydiumLaunchpadPlatformCurveParam {
    pub epoch: u64,
    pub index: u8,
    pub global_config: Pubkey,
    pub bonding_curve_param: RaydiumLaunchpadBondingCurveParam,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RaydiumLaunchpadPoolCreateEvent {
    pub pool_state: Pubkey,
    pub creator: Pubkey,
    pub config: Pubkey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RaydiumLaunchpadTradeEvent {
    pub pool_state: Pubkey,
    pub total_base_sell: u64,
    pub virtual_base: u64,
    pub virtual_quote: u64,
    pub real_base_before: u64,
    pub real_quote_before: u64,
    pub real_base_after: u64,
    pub real_quote_after: u64,
    pub amount_in: u64,
    pub amount_out: u64,
    pub protocol_fee: u64,
    pub platform_fee: u64,
    pub creator_fee: u64,
    pub share_fee: u64,
    pub trade_direction: u8,
    pub pool_status: u8,
    pub exact_in: bool,
}

#[derive(Debug)]
pub enum RaydiumLaunchpadEvent {
    PoolCreate(Option<RaydiumLaunchpadPoolCreateEvent>),
    Trade(Option<RaydiumLaunchpadTradeEvent>),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchpadSwapInstructionKind {
    BuyExactIn,
    BuyExactOut,
    SellExactIn,
    SellExactOut,
}

#[derive(Clone)]
pub struct RaydiumLaunchpad {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
}

impl RaydiumLaunchpadTradeEvent {
    fn deserialize_from_cursor(cursor: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        Ok(Self {
            pool_state: RaydiumLaunchpad::read_pubkey_from_cursor(cursor)?,
            total_base_sell: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            virtual_base: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            virtual_quote: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            real_base_before: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            real_quote_before: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            real_base_after: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            real_quote_after: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            amount_in: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            amount_out: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            protocol_fee: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            platform_fee: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            creator_fee: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            share_fee: RaydiumLaunchpad::read_u64_from_cursor(cursor)?,
            trade_direction: RaydiumLaunchpad::read_u8_from_cursor(cursor)?,
            pool_status: RaydiumLaunchpad::read_u8_from_cursor(cursor)?,
            exact_in: RaydiumLaunchpad::read_u8_from_cursor(cursor)? != 0,
        })
    }
}

impl RaydiumLaunchpad {
    pub fn new(keypair: Arc<Keypair>, sol: Arc<SolHook>) -> Self {
        Self { keypair, sol }
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

    fn read_u8(data: &[u8], offset: usize) -> anyhow::Result<u8> {
        data.get(offset)
            .copied()
            .with_context(|| format!("missing u8 byte at offset {offset}"))
    }

    fn read_fixed_str(data: &[u8], offset: usize, len: usize) -> anyhow::Result<String> {
        let bytes = data
            .get(offset..offset + len)
            .with_context(|| format!("missing string bytes at offset {offset} (len {len})"))?;
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        Ok(String::from_utf8_lossy(&bytes[..end]).trim().to_string())
    }

    fn read_pubkey_from_cursor(cursor: &mut Cursor<&[u8]>) -> anyhow::Result<Pubkey> {
        let mut bytes = [0u8; 32];
        cursor
            .read_exact(&mut bytes)
            .context("failed to read pubkey from event payload")?;
        Ok(Pubkey::new_from_array(bytes))
    }

    fn read_u64_from_cursor(cursor: &mut Cursor<&[u8]>) -> anyhow::Result<u64> {
        let mut bytes = [0u8; 8];
        cursor
            .read_exact(&mut bytes)
            .context("failed to read u64 from event payload")?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn read_u8_from_cursor(cursor: &mut Cursor<&[u8]>) -> anyhow::Result<u8> {
        let mut bytes = [0u8; 1];
        cursor
            .read_exact(&mut bytes)
            .context("failed to read u8 from event payload")?;
        Ok(bytes[0])
    }

    fn read_u32_from_cursor(cursor: &mut Cursor<&[u8]>) -> anyhow::Result<u32> {
        let mut bytes = [0u8; 4];
        cursor
            .read_exact(&mut bytes)
            .context("failed to read u32 from payload")?;
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn decode_pool_state_account_data(
        data: &[u8],
    ) -> anyhow::Result<RaydiumLaunchpadPoolState> {
        anyhow::ensure!(
            data.len() >= LAUNCHPAD_POOL_ACCOUNT_LEN,
            "raydium launchpad pool account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == POOL_STATE_DISCRIM,
            "raydium launchpad pool discriminator mismatch"
        );

        Ok(RaydiumLaunchpadPoolState {
            epoch: Self::read_u64(data, LAUNCHPAD_POOL_EPOCH_OFFSET)?,
            auth_bump: Self::read_u8(data, LAUNCHPAD_POOL_AUTH_BUMP_OFFSET)?,
            status: Self::read_u8(data, LAUNCHPAD_POOL_STATUS_OFFSET)?,
            base_decimals: Self::read_u8(data, LAUNCHPAD_POOL_BASE_DECIMALS_OFFSET)?,
            quote_decimals: Self::read_u8(data, LAUNCHPAD_POOL_QUOTE_DECIMALS_OFFSET)?,
            migrate_type: Self::read_u8(data, LAUNCHPAD_POOL_MIGRATE_TYPE_OFFSET)?,
            supply: Self::read_u64(data, LAUNCHPAD_POOL_SUPPLY_OFFSET)?,
            total_base_sell: Self::read_u64(data, LAUNCHPAD_POOL_TOTAL_BASE_SELL_OFFSET)?,
            virtual_base: Self::read_u64(data, LAUNCHPAD_POOL_VIRTUAL_BASE_OFFSET)?,
            virtual_quote: Self::read_u64(data, LAUNCHPAD_POOL_VIRTUAL_QUOTE_OFFSET)?,
            real_base: Self::read_u64(data, LAUNCHPAD_POOL_REAL_BASE_OFFSET)?,
            real_quote: Self::read_u64(data, LAUNCHPAD_POOL_REAL_QUOTE_OFFSET)?,
            total_quote_fund_raising: Self::read_u64(
                data,
                LAUNCHPAD_POOL_TOTAL_QUOTE_FUND_RAISING_OFFSET,
            )?,
            quote_protocol_fee: Self::read_u64(data, LAUNCHPAD_POOL_QUOTE_PROTOCOL_FEE_OFFSET)?,
            platform_fee: Self::read_u64(data, LAUNCHPAD_POOL_PLATFORM_FEE_OFFSET)?,
            migrate_fee: Self::read_u64(data, LAUNCHPAD_POOL_MIGRATE_FEE_OFFSET)?,
            vesting_schedule: RaydiumLaunchpadVestingSchedule {
                total_locked_amount: Self::read_u64(
                    data,
                    LAUNCHPAD_POOL_VESTING_TOTAL_LOCKED_AMOUNT_OFFSET,
                )?,
                cliff_period: Self::read_u64(data, LAUNCHPAD_POOL_VESTING_CLIFF_PERIOD_OFFSET)?,
                unlock_period: Self::read_u64(data, LAUNCHPAD_POOL_VESTING_UNLOCK_PERIOD_OFFSET)?,
                start_time: Self::read_u64(data, LAUNCHPAD_POOL_VESTING_START_TIME_OFFSET)?,
                allocated_share_amount: Self::read_u64(
                    data,
                    LAUNCHPAD_POOL_VESTING_ALLOCATED_SHARE_AMOUNT_OFFSET,
                )?,
            },
            global_config: Self::read_pubkey(data, LAUNCHPAD_POOL_GLOBAL_CONFIG_OFFSET)?,
            platform_config: Self::read_pubkey(data, LAUNCHPAD_POOL_PLATFORM_CONFIG_OFFSET)?,
            base_mint: Self::read_pubkey(data, LAUNCHPAD_POOL_BASE_MINT_OFFSET)?,
            quote_mint: Self::read_pubkey(data, LAUNCHPAD_POOL_QUOTE_MINT_OFFSET)?,
            base_vault: Self::read_pubkey(data, LAUNCHPAD_POOL_BASE_VAULT_OFFSET)?,
            quote_vault: Self::read_pubkey(data, LAUNCHPAD_POOL_QUOTE_VAULT_OFFSET)?,
            creator: Self::read_pubkey(data, LAUNCHPAD_POOL_CREATOR_OFFSET)?,
            token_program_flag: Self::read_u8(data, LAUNCHPAD_POOL_TOKEN_PROGRAM_FLAG_OFFSET)?,
            amm_fee_on: Self::read_u8(data, LAUNCHPAD_POOL_AMM_FEE_ON_OFFSET)?,
        })
    }

    pub fn decode_global_config_account_data(
        data: &[u8],
    ) -> anyhow::Result<RaydiumLaunchpadGlobalConfigState> {
        anyhow::ensure!(
            data.len() >= LAUNCHPAD_GLOBAL_CONFIG_ACCOUNT_LEN,
            "raydium launchpad global config account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == GLOBAL_CONFIG_DISCRIM,
            "raydium launchpad global config discriminator mismatch"
        );

        Ok(RaydiumLaunchpadGlobalConfigState {
            curve_type: Self::read_u8(data, LAUNCHPAD_GLOBAL_CONFIG_CURVE_TYPE_OFFSET)?,
            trade_fee_rate: Self::read_u64(data, LAUNCHPAD_GLOBAL_CONFIG_TRADE_FEE_RATE_OFFSET)?,
            max_share_fee_rate: Self::read_u64(
                data,
                LAUNCHPAD_GLOBAL_CONFIG_MAX_SHARE_FEE_RATE_OFFSET,
            )?,
            min_base_supply: Self::read_u64(data, LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_SUPPLY_OFFSET)?,
            max_lock_rate: Self::read_u64(data, LAUNCHPAD_GLOBAL_CONFIG_MAX_LOCK_RATE_OFFSET)?,
            min_base_sell_rate: Self::read_u64(
                data,
                LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_SELL_RATE_OFFSET,
            )?,
            min_base_migrate_rate: Self::read_u64(
                data,
                LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_MIGRATE_RATE_OFFSET,
            )?,
            min_quote_fund_raising: Self::read_u64(
                data,
                LAUNCHPAD_GLOBAL_CONFIG_MIN_QUOTE_FUND_RAISING_OFFSET,
            )?,
            quote_mint: Self::read_pubkey(data, LAUNCHPAD_GLOBAL_CONFIG_QUOTE_MINT_OFFSET)?,
        })
    }

    pub fn decode_platform_config_account_data(
        data: &[u8],
    ) -> anyhow::Result<RaydiumLaunchpadPlatformConfigState> {
        anyhow::ensure!(
            data.len() >= LAUNCHPAD_PLATFORM_CONFIG_ACCOUNT_MIN_LEN,
            "raydium launchpad platform config account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == PLATFORM_CONFIG_DISCRIM,
            "raydium launchpad platform config discriminator mismatch"
        );

        Ok(RaydiumLaunchpadPlatformConfigState {
            platform_fee_wallet: Self::read_pubkey(
                data,
                LAUNCHPAD_PLATFORM_CONFIG_PLATFORM_FEE_WALLET_OFFSET,
            )?,
            fee_rate: Self::read_u64(data, LAUNCHPAD_PLATFORM_CONFIG_FEE_RATE_OFFSET)?,
            creator_fee_rate: Self::read_u64(
                data,
                LAUNCHPAD_PLATFORM_CONFIG_CREATOR_FEE_RATE_OFFSET,
            )?,
        })
    }

    pub fn decode_platform_config_info(
        data: &[u8],
    ) -> anyhow::Result<RaydiumLaunchpadPlatformConfigInfo> {
        anyhow::ensure!(
            data.len() >= LAUNCHPAD_PLATFORM_CONFIG_ACCOUNT_MIN_LEN,
            "raydium launchpad platform config account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == PLATFORM_CONFIG_DISCRIM,
            "raydium launchpad platform config discriminator mismatch"
        );

        Ok(RaydiumLaunchpadPlatformConfigInfo {
            name: Self::read_fixed_str(
                data,
                LAUNCHPAD_PLATFORM_CONFIG_NAME_OFFSET,
                LAUNCHPAD_PLATFORM_CONFIG_NAME_LEN,
            )?,
            web: Self::read_fixed_str(
                data,
                LAUNCHPAD_PLATFORM_CONFIG_WEB_OFFSET,
                LAUNCHPAD_PLATFORM_CONFIG_WEB_LEN,
            )?,
            img: Self::read_fixed_str(
                data,
                LAUNCHPAD_PLATFORM_CONFIG_IMG_OFFSET,
                LAUNCHPAD_PLATFORM_CONFIG_IMG_LEN,
            )?,
        })
    }

    pub fn decode_platform_curve_params(
        data: &[u8],
    ) -> anyhow::Result<Vec<RaydiumLaunchpadPlatformCurveParam>> {
        anyhow::ensure!(
            data.len() >= LAUNCHPAD_PLATFORM_CONFIG_CURVE_PARAMS_OFFSET + 4,
            "raydium launchpad platform config too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == PLATFORM_CONFIG_DISCRIM,
            "raydium launchpad platform config discriminator mismatch"
        );

        let mut cursor = Cursor::new(data);
        cursor.set_position(LAUNCHPAD_PLATFORM_CONFIG_CURVE_PARAMS_OFFSET as u64);
        let len = Self::read_u32_from_cursor(&mut cursor)? as usize;
        anyhow::ensure!(
            len <= 1024,
            "raydium launchpad curve params too large: {len}"
        );

        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            let epoch = Self::read_u64_from_cursor(&mut cursor)?;
            let index = Self::read_u8_from_cursor(&mut cursor)?;
            let global_config = Self::read_pubkey_from_cursor(&mut cursor)?;

            let migrate_type = Self::read_u8_from_cursor(&mut cursor)?;
            let amm_fee_on = Self::read_u8_from_cursor(&mut cursor)?;
            let supply = Self::read_u64_from_cursor(&mut cursor)?;
            let total_base_sell = Self::read_u64_from_cursor(&mut cursor)?;
            let total_quote_fund_raising = Self::read_u64_from_cursor(&mut cursor)?;
            let total_locked_amount = Self::read_u64_from_cursor(&mut cursor)?;
            let cliff_period = Self::read_u64_from_cursor(&mut cursor)?;
            let unlock_period = Self::read_u64_from_cursor(&mut cursor)?;

            // Skip padding [u64; 50].
            cursor.set_position(cursor.position().saturating_add(8 * 50));

            out.push(RaydiumLaunchpadPlatformCurveParam {
                epoch,
                index,
                global_config,
                bonding_curve_param: RaydiumLaunchpadBondingCurveParam {
                    migrate_type,
                    amm_fee_on,
                    supply,
                    total_base_sell,
                    total_quote_fund_raising,
                    total_locked_amount,
                    cliff_period,
                    unlock_period,
                },
            });
        }

        Ok(out)
    }

    fn derive_authority_pda(program_id: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[LAUNCHPAD_AUTH_SEED], program_id).0
    }

    fn derive_event_authority_pda(program_id: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[LAUNCHPAD_EVENT_AUTH_SEED], program_id).0
    }

    fn derive_platform_fee_vault(
        program_id: &Pubkey,
        platform_config: &Pubkey,
        quote_mint: &Pubkey,
    ) -> Pubkey {
        Pubkey::find_program_address(&[platform_config.as_ref(), quote_mint.as_ref()], program_id).0
    }

    fn derive_creator_fee_vault(
        program_id: &Pubkey,
        creator: &Pubkey,
        quote_mint: &Pubkey,
    ) -> Pubkey {
        Pubkey::find_program_address(&[creator.as_ref(), quote_mint.as_ref()], program_id).0
    }

    fn decode_swap_instruction_kind(data: &[u8]) -> Option<LaunchpadSwapInstructionKind> {
        let discrim = data.get(..8)?;
        match discrim {
            value if value == BUY_EXACT_IN_IX_DISCRIM => {
                Some(LaunchpadSwapInstructionKind::BuyExactIn)
            }
            value if value == BUY_EXACT_OUT_IX_DISCRIM => {
                Some(LaunchpadSwapInstructionKind::BuyExactOut)
            }
            value if value == SELL_EXACT_IN_IX_DISCRIM => {
                Some(LaunchpadSwapInstructionKind::SellExactIn)
            }
            value if value == SELL_EXACT_OUT_IX_DISCRIM => {
                Some(LaunchpadSwapInstructionKind::SellExactOut)
            }
            _ => None,
        }
    }

    fn decode_swap_instruction_kind_base58(data: &str) -> Option<LaunchpadSwapInstructionKind> {
        let bytes = bs58::decode(data).into_vec().ok()?;
        Self::decode_swap_instruction_kind(&bytes)
    }

    fn swap_user_source_index(kind: LaunchpadSwapInstructionKind) -> usize {
        match kind {
            LaunchpadSwapInstructionKind::BuyExactIn
            | LaunchpadSwapInstructionKind::BuyExactOut => 6,
            LaunchpadSwapInstructionKind::SellExactIn
            | LaunchpadSwapInstructionKind::SellExactOut => 5,
        }
    }

    fn swap_program_data_kind_from_parsed(
        parsed: &UiParsedInstruction,
    ) -> Option<LaunchpadSwapInstructionKind> {
        let UiParsedInstruction::Parsed(value) = parsed else {
            return None;
        };
        let kind_name = value
            .parsed
            .get("type")
            .and_then(|kind| kind.as_str())
            .map(|s| s.to_ascii_lowercase())?;
        if kind_name.contains("buy") && kind_name.contains("exact") && kind_name.contains("in") {
            return Some(LaunchpadSwapInstructionKind::BuyExactIn);
        }
        if kind_name.contains("buy") && kind_name.contains("exact") && kind_name.contains("out") {
            return Some(LaunchpadSwapInstructionKind::BuyExactOut);
        }
        if kind_name.contains("sell") && kind_name.contains("exact") && kind_name.contains("in") {
            return Some(LaunchpadSwapInstructionKind::SellExactIn);
        }
        if kind_name.contains("sell") && kind_name.contains("exact") && kind_name.contains("out") {
            return Some(LaunchpadSwapInstructionKind::SellExactOut);
        }
        None
    }

    fn encode_buy_exact_in_instruction_data(
        amount_in_quote: u64,
        minimum_amount_out_base: u64,
        share_fee_rate: u64,
    ) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + 8 + 8 + 8);
        data.extend_from_slice(&BUY_EXACT_IN_IX_DISCRIM);
        data.extend_from_slice(&amount_in_quote.to_le_bytes());
        data.extend_from_slice(&minimum_amount_out_base.to_le_bytes());
        data.extend_from_slice(&share_fee_rate.to_le_bytes());
        data
    }

    fn encode_sell_exact_in_instruction_data(
        amount_in_base: u64,
        minimum_amount_out_quote: u64,
        share_fee_rate: u64,
    ) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + 8 + 8 + 8);
        data.extend_from_slice(&SELL_EXACT_IN_IX_DISCRIM);
        data.extend_from_slice(&amount_in_base.to_le_bytes());
        data.extend_from_slice(&minimum_amount_out_quote.to_le_bytes());
        data.extend_from_slice(&share_fee_rate.to_le_bytes());
        data
    }

    fn ata_for(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(owner, mint, token_program)
    }

    fn token_program_for_mint(
        state: &RaydiumLaunchpadPoolState,
        mint: &Pubkey,
    ) -> anyhow::Result<Pubkey> {
        if *mint == state.base_mint {
            return Ok(if state.token_program_flag & 0b0000_0001 == 0 {
                TOKEN_PROGRAM_ID
            } else {
                spl_token_2022::id()
            });
        }
        if *mint == state.quote_mint {
            return Ok(if (state.token_program_flag >> 1) & 0b0000_0001 == 0 {
                TOKEN_PROGRAM_ID
            } else {
                spl_token_2022::id()
            });
        }
        anyhow::bail!("mint {} not present in launchpad pool", mint)
    }

    fn validate_pool_contains_mint(
        state: &RaydiumLaunchpadPoolState,
        mint: &Pubkey,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.base_mint == *mint || state.quote_mint == *mint,
            "raydium launchpad pool does not contain mint {}",
            mint
        );
        anyhow::ensure!(
            state.base_mint == WSOL_MINT || state.quote_mint == WSOL_MINT,
            "raydium launchpad pool is not WSOL-quoted"
        );
        Ok(())
    }

    fn validate_swap_enabled(state: &RaydiumLaunchpadPoolState) -> anyhow::Result<()> {
        match state.status {
            LAUNCHPAD_POOL_STATUS_FUND => Ok(()),
            LAUNCHPAD_POOL_STATUS_MIGRATE => anyhow::bail!(
                "raydium launchpad pool status is MIGRATE (1): funding ended and pool is awaiting migration; launchpad swaps are disabled"
            ),
            LAUNCHPAD_POOL_STATUS_TRADE => {
                let target = match state.migrate_type {
                    0 => "raydium_amm_v4",
                    1 => "raydium_cpmm",
                    _ => "raydium_amm_v4/raydium_cpmm",
                };
                anyhow::bail!(
                    "raydium launchpad pool status is TRADE (2): migration complete; trade this mint on {target} instead of raydium_launchpad"
                )
            }
            other => anyhow::bail!("raydium launchpad pool status {} is invalid", other),
        }
    }

    fn total_fee_rate(
        global: &RaydiumLaunchpadGlobalConfigState,
        platform: &RaydiumLaunchpadPlatformConfigState,
        share_fee_rate: u64,
    ) -> anyhow::Result<u64> {
        let total = global
            .trade_fee_rate
            .saturating_add(platform.fee_rate)
            .saturating_add(platform.creator_fee_rate)
            .saturating_add(share_fee_rate);
        anyhow::ensure!(
            total <= FEE_RATE_DENOMINATOR,
            "raydium launchpad total fee rate {} exceeds denominator {}",
            total,
            FEE_RATE_DENOMINATOR
        );
        anyhow::ensure!(
            share_fee_rate <= global.max_share_fee_rate,
            "share fee rate {} exceeds global max {}",
            share_fee_rate,
            global.max_share_fee_rate
        );
        Ok(total)
    }

    fn ceil_div_u128(numerator: u128, denominator: u128) -> anyhow::Result<u128> {
        anyhow::ensure!(denominator > 0, "division by zero");
        Ok(numerator.div_ceil(denominator))
    }

    fn calculate_fee(amount: u64, fee_rate: u64) -> anyhow::Result<u64> {
        if amount == 0 || fee_rate == 0 {
            return Ok(0);
        }
        let numerator = (amount as u128)
            .checked_mul(fee_rate as u128)
            .context("launchpad fee multiplication overflow")?;
        let fee = Self::ceil_div_u128(numerator, FEE_RATE_DENOMINATOR as u128)?;
        u64::try_from(fee).context("launchpad fee overflow")
    }

    fn integer_sqrt_u128(value: u128) -> u128 {
        if value <= 1 {
            return value;
        }
        let mut x0 = value;
        let mut x1 = (x0 + value / x0) >> 1;
        while x1 < x0 {
            x0 = x1;
            x1 = (x0 + value / x0) >> 1;
        }
        x0
    }

    fn curve_buy_exact_in_output_raw(
        state: &RaydiumLaunchpadPoolState,
        curve_type: u8,
        amount_quote_in_raw: u64,
    ) -> anyhow::Result<u64> {
        if amount_quote_in_raw == 0 {
            return Ok(0);
        }

        let output = match curve_type {
            0 => {
                let input_reserve = state.virtual_quote as u128 + state.real_quote as u128;
                let output_reserve = state.virtual_base.saturating_sub(state.real_base) as u128;
                anyhow::ensure!(
                    input_reserve > 0 && output_reserve > 0,
                    "raydium launchpad constant-product reserves are empty"
                );
                let numerator = (amount_quote_in_raw as u128)
                    .checked_mul(output_reserve)
                    .context("launchpad buy output numerator overflow")?;
                let denominator = input_reserve + amount_quote_in_raw as u128;
                numerator / denominator
            }
            1 => {
                anyhow::ensure!(
                    state.virtual_quote > 0 && state.virtual_base > 0,
                    "raydium launchpad fixed-price virtual reserves are empty"
                );
                let numerator = (amount_quote_in_raw as u128)
                    .checked_mul(state.virtual_base as u128)
                    .context("launchpad fixed-price buy numerator overflow")?;
                numerator / state.virtual_quote as u128
            }
            2 => {
                anyhow::ensure!(
                    state.virtual_base > 0,
                    "raydium launchpad linear-price virtual base is zero"
                );
                let new_quote = state.real_quote as u128 + amount_quote_in_raw as u128;
                let term = (2u128)
                    .checked_mul(new_quote)
                    .and_then(|value| value.checked_mul(LINEAR_Q64))
                    .context("launchpad linear buy term overflow")?
                    / state.virtual_base as u128;
                let sqrt_term = Self::integer_sqrt_u128(term);

                sqrt_term.saturating_sub(state.real_base as u128)
            }
            other => anyhow::bail!("unsupported raydium launchpad curve type {}", other),
        };

        u64::try_from(output).context("launchpad buy output overflow")
    }

    fn curve_sell_exact_in_output_raw(
        state: &RaydiumLaunchpadPoolState,
        curve_type: u8,
        amount_base_in_raw: u64,
    ) -> anyhow::Result<u64> {
        if amount_base_in_raw == 0 {
            return Ok(0);
        }

        let output = match curve_type {
            0 => {
                let input_reserve = state.virtual_base.saturating_sub(state.real_base) as u128;
                let output_reserve = state.virtual_quote as u128 + state.real_quote as u128;
                anyhow::ensure!(
                    input_reserve > 0 && output_reserve > 0,
                    "raydium launchpad constant-product reserves are empty"
                );
                let numerator = (amount_base_in_raw as u128)
                    .checked_mul(output_reserve)
                    .context("launchpad sell output numerator overflow")?;
                let denominator = input_reserve + amount_base_in_raw as u128;
                numerator / denominator
            }
            1 => {
                anyhow::ensure!(
                    state.virtual_base > 0 && state.virtual_quote > 0,
                    "raydium launchpad fixed-price virtual reserves are empty"
                );
                let numerator = (amount_base_in_raw as u128)
                    .checked_mul(state.virtual_quote as u128)
                    .context("launchpad fixed-price sell numerator overflow")?;
                numerator / state.virtual_base as u128
            }
            2 => {
                anyhow::ensure!(
                    amount_base_in_raw <= state.real_base,
                    "raydium launchpad linear sell amount exceeds real base reserve"
                );
                let new_base = state.real_base as u128 - amount_base_in_raw as u128;
                let new_base_squared = new_base
                    .checked_mul(new_base)
                    .context("launchpad linear sell square overflow")?;
                let numerator = (state.virtual_base as u128)
                    .checked_mul(new_base_squared)
                    .context("launchpad linear sell numerator overflow")?;
                let denominator = 2u128
                    .checked_mul(LINEAR_Q64)
                    .context("launchpad linear sell denominator overflow")?;
                let new_quote = Self::ceil_div_u128(numerator, denominator)?;
                (state.real_quote as u128).saturating_sub(new_quote)
            }
            other => anyhow::bail!("unsupported raydium launchpad curve type {}", other),
        };

        u64::try_from(output).context("launchpad sell output overflow")
    }

    fn quote_buy_amount_out_raw(
        state: &RaydiumLaunchpadPoolState,
        global: &RaydiumLaunchpadGlobalConfigState,
        platform: &RaydiumLaunchpadPlatformConfigState,
        amount_quote_in_raw: u64,
        share_fee_rate: u64,
    ) -> anyhow::Result<u64> {
        let total_fee_rate = Self::total_fee_rate(global, platform, share_fee_rate)?;
        let total_fee = Self::calculate_fee(amount_quote_in_raw, total_fee_rate)?;
        let amount_less_fee = amount_quote_in_raw.saturating_sub(total_fee);
        let mut amount_out =
            Self::curve_buy_exact_in_output_raw(state, global.curve_type, amount_less_fee)?;
        let remaining_amount = state.total_base_sell.saturating_sub(state.real_base);
        if amount_out > remaining_amount {
            amount_out = remaining_amount;
        }
        Ok(amount_out)
    }

    fn quote_sell_amount_out_raw(
        state: &RaydiumLaunchpadPoolState,
        global: &RaydiumLaunchpadGlobalConfigState,
        platform: &RaydiumLaunchpadPlatformConfigState,
        amount_base_in_raw: u64,
        share_fee_rate: u64,
    ) -> anyhow::Result<u64> {
        let amount_out_before_fee =
            Self::curve_sell_exact_in_output_raw(state, global.curve_type, amount_base_in_raw)?;
        let total_fee_rate = Self::total_fee_rate(global, platform, share_fee_rate)?;
        let total_fee = Self::calculate_fee(amount_out_before_fee, total_fee_rate)?;
        Ok(amount_out_before_fee.saturating_sub(total_fee))
    }

    fn min_output_after_slippage(expected_out_raw: u64, slippage_pct: f64) -> u64 {
        if expected_out_raw == 0 {
            return 0;
        }
        let min_output = ((expected_out_raw as f64) * (1.0 - slippage_pct))
            .max(0.0)
            .floor() as u64;
        min_output.max(1)
    }

    pub fn sol_price_from_pool_state(
        state: &RaydiumLaunchpadPoolState,
        curve_type: u8,
    ) -> anyhow::Result<f64> {
        let decimal_factor = 10_f64.powi(state.base_decimals as i32 - state.quote_decimals as i32);

        let computed = match curve_type {
            0 => {
                let numerator = state.virtual_quote as f64 + state.real_quote as f64;
                let denominator = (state.virtual_base as f64) - (state.real_base as f64);
                if denominator > 0.0 {
                    Some((numerator / denominator) * decimal_factor)
                } else {
                    None
                }
            }
            1 => {
                if state.virtual_base > 0 {
                    Some((state.virtual_quote as f64 / state.virtual_base as f64) * decimal_factor)
                } else {
                    None
                }
            }
            2 => {
                let raw =
                    (state.virtual_base as f64 * state.real_base as f64) / (LINEAR_Q64 as f64);
                if raw > 0.0 {
                    Some(raw * decimal_factor)
                } else {
                    None
                }
            }
            _ => None,
        };

        let fallback = if state.total_base_sell > 0 {
            (state.total_quote_fund_raising as f64 / state.total_base_sell as f64) * decimal_factor
        } else {
            0.0
        };

        let price = computed
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(fallback);
        anyhow::ensure!(
            price.is_finite() && price > 0.0,
            "invalid raydium launchpad price for curve type {}",
            curve_type
        );
        Ok(price)
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

    async fn user_token_balance_raw(
        &self,
        owner: &Pubkey,
        mint: &Pubkey,
        token_program: &Pubkey,
    ) -> anyhow::Result<u64> {
        let ata = Self::ata_for(owner, mint, token_program);
        self.token_balance_raw(&ata).await
    }

    fn validate_launchpad_swap_pair(
        state: &RaydiumLaunchpadPoolState,
        mint: &Pubkey,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.base_mint == *mint,
            "raydium launchpad swap expects base mint {}, got {}",
            state.base_mint,
            mint
        );
        anyhow::ensure!(
            state.quote_mint == WSOL_MINT,
            "raydium launchpad quote mint must be WSOL for SOL swap path"
        );
        Ok(())
    }

    async fn fetch_pool_configs(
        &self,
        program_id: &Pubkey,
        state: &RaydiumLaunchpadPoolState,
    ) -> anyhow::Result<(
        RaydiumLaunchpadGlobalConfigState,
        RaydiumLaunchpadPlatformConfigState,
    )> {
        let global_acc = self
            .sol
            .rpc_client
            .get_account_with_commitment(&state.global_config, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!(
                "raydium launchpad global config account not found"
            ))?;
        anyhow::ensure!(
            global_acc.owner == *program_id,
            "raydium launchpad global config owner {} does not match program {}",
            global_acc.owner,
            program_id
        );
        let global_config = Self::decode_global_config_account_data(&global_acc.data)?;

        let platform_acc = self
            .sol
            .rpc_client
            .get_account_with_commitment(&state.platform_config, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!(
                "raydium launchpad platform config account not found"
            ))?;
        anyhow::ensure!(
            platform_acc.owner == *program_id,
            "raydium launchpad platform config owner {} does not match program {}",
            platform_acc.owner,
            program_id
        );
        let platform_config = Self::decode_platform_config_account_data(&platform_acc.data)?;
        anyhow::ensure!(
            global_config.quote_mint == state.quote_mint,
            "raydium launchpad global config quote mint mismatch"
        );
        Ok((global_config, platform_config))
    }

    fn expected_authority_pda(program_id: &Pubkey) -> anyhow::Result<Pubkey> {
        if *program_id == RAYDIUM_LAUNCHPAD_ID {
            return Ok(RAYDIUM_LAUNCHPAD_AUTH);
        }
        if *program_id == RAYDIUM_LAUNCHPAD_DEVNET_ID {
            return Ok(RAYDIUM_LAUNCHPAD_DEVNET_AUTH);
        }
        anyhow::bail!("unsupported raydium launchpad program id {}", program_id);
    }

    fn build_swap_accounts(
        program_id: &Pubkey,
        state: &RaydiumLaunchpadPoolState,
        pool: &Pubkey,
        payer: &Pubkey,
        user_base_token: Pubkey,
        user_quote_token: Pubkey,
        share_fee_rate: u64,
        share_fee_receiver: Option<Pubkey>,
    ) -> anyhow::Result<Vec<AccountMeta>> {
        let base_program = Self::token_program_for_mint(state, &state.base_mint)?;
        let quote_program = Self::token_program_for_mint(state, &state.quote_mint)?;
        let authority = Self::derive_authority_pda(program_id);
        let expected_authority = Self::expected_authority_pda(program_id)?;
        anyhow::ensure!(
            authority == expected_authority,
            "raydium launchpad authority PDA mismatch for program {}: got {}, expected {}",
            program_id,
            authority,
            expected_authority
        );
        let event_authority = Self::derive_event_authority_pda(program_id);
        let platform_fee_vault =
            Self::derive_platform_fee_vault(program_id, &state.platform_config, &state.quote_mint);
        let creator_fee_vault =
            Self::derive_creator_fee_vault(program_id, &state.creator, &state.quote_mint);

        let mut accounts = vec![
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new_readonly(state.global_config, false),
            AccountMeta::new_readonly(state.platform_config, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(user_base_token, false),
            AccountMeta::new(user_quote_token, false),
            AccountMeta::new(state.base_vault, false),
            AccountMeta::new(state.quote_vault, false),
            AccountMeta::new_readonly(state.base_mint, false),
            AccountMeta::new_readonly(state.quote_mint, false),
            AccountMeta::new_readonly(base_program, false),
            AccountMeta::new_readonly(quote_program, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(*program_id, false),
        ];

        if share_fee_rate > 0 {
            let receiver = share_fee_receiver.context(
                "raydium launchpad share fee receiver is required when share_fee_rate > 0",
            )?;
            accounts.push(AccountMeta::new(receiver, false));
        }

        accounts.push(AccountMeta::new_readonly(SYSTEM_PROGRAM, false));
        accounts.push(AccountMeta::new(platform_fee_vault, false));
        accounts.push(AccountMeta::new(creator_fee_vault, false));

        Ok(accounts)
    }

    pub fn parse_logs(
        logs: std::slice::Iter<'_, String>,
        sig: Option<&String>,
    ) -> Vec<RaydiumLaunchpadEvent> {
        let mut events = Vec::new();
        let sig_text = sig.map(String::as_str).unwrap_or("");

        for log in logs {
            let Some(prefix_idx) = log.find(SEARCH_FOR_PROGRAM_DATA) else {
                continue;
            };
            let payload = log[prefix_idx + SEARCH_FOR_PROGRAM_DATA.len()..].trim();
            let bytes = match decode_b64(payload) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if bytes.len() < 8 {
                continue;
            }
            let Some(discrim) = bytes.get(..8) else {
                continue;
            };
            let event_bytes = &bytes[8..];

            if discrim == TRADE_EVENT_DISCRIM {
                let mut cursor = Cursor::new(event_bytes);
                match RaydiumLaunchpadTradeEvent::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(RaydiumLaunchpadEvent::Trade(Some(event))),
                    Err(err) => warn!(
                        "Error deserializing raydium launchpad trade event {:?}: {err}",
                        sig_text
                    ),
                }
                continue;
            }

            if discrim == POOL_CREATE_EVENT_DISCRIM {
                if event_bytes.len() < 96 {
                    continue;
                }
                let mut cursor = Cursor::new(event_bytes);
                let pool_state = match Self::read_pubkey_from_cursor(&mut cursor) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                let creator = match Self::read_pubkey_from_cursor(&mut cursor) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                let config = match Self::read_pubkey_from_cursor(&mut cursor) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                events.push(RaydiumLaunchpadEvent::PoolCreate(Some(
                    RaydiumLaunchpadPoolCreateEvent {
                        pool_state,
                        creator,
                        config,
                    },
                )));
                continue;
            }

            events.push(RaydiumLaunchpadEvent::Unknown);
        }

        events
    }

    pub fn extract_pool_from_transaction(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> Option<Pubkey> {
        fn extract_pool_from_instruction(
            ix: &UiInstruction,
            program_ids: &[&str],
            account_keys: &[&str],
        ) -> Option<Pubkey> {
            match ix {
                UiInstruction::Parsed(UiParsedInstruction::PartiallyDecoded(decoded)) => {
                    if !program_ids
                        .iter()
                        .any(|program_id| decoded.program_id == *program_id)
                    {
                        return None;
                    }
                    let kind =
                        RaydiumLaunchpad::decode_swap_instruction_kind_base58(&decoded.data)?;
                    let _ = kind;
                    decoded
                        .accounts
                        .get(4)
                        .and_then(|value| Pubkey::from_str(value).ok())
                }
                UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed)) => {
                    if !program_ids
                        .iter()
                        .any(|program_id| parsed.program_id == *program_id)
                    {
                        return None;
                    }
                    RaydiumLaunchpad::swap_program_data_kind_from_parsed(
                        &UiParsedInstruction::Parsed(parsed.clone()),
                    )?;
                    let info = parsed.parsed.get("info")?;
                    for key in ["poolState", "pool_state", "pool", "poolId", "pool_id"] {
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
                    if !program_ids.contains(program) {
                        return None;
                    }
                    RaydiumLaunchpad::decode_swap_instruction_kind_base58(&compiled.data)?;
                    let pool_idx = *compiled.accounts.get(4)? as usize;
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
            RAYDIUM_LAUNCHPAD_ID.to_string(),
            RAYDIUM_LAUNCHPAD_DEVNET_ID.to_string(),
        ];
        let program_id_texts = [program_ids[0].as_str(), program_ids[1].as_str()];
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        for ix in &msg.instructions {
            if let Some(pool) = extract_pool_from_instruction(ix, &program_id_texts, &account_keys)
            {
                return Some(pool);
            }
        }

        let Some(meta) = tx.transaction.meta.as_ref() else {
            return None;
        };
        let OptionSerializer::Some(inner_instructions) = &meta.inner_instructions else {
            return None;
        };
        for inner in inner_instructions {
            for ix in &inner.instructions {
                if let Some(pool) =
                    extract_pool_from_instruction(ix, &program_id_texts, &account_keys)
                {
                    return Some(pool);
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
            program_ids: &[&str],
            account_keys: &[&str],
        ) -> Option<Pubkey> {
            match ix {
                UiInstruction::Parsed(UiParsedInstruction::PartiallyDecoded(decoded)) => {
                    if !program_ids
                        .iter()
                        .any(|program_id| decoded.program_id == *program_id)
                    {
                        return None;
                    }
                    let kind =
                        RaydiumLaunchpad::decode_swap_instruction_kind_base58(&decoded.data)?;
                    let source_idx = RaydiumLaunchpad::swap_user_source_index(kind);
                    decoded
                        .accounts
                        .get(source_idx)
                        .and_then(|value| Pubkey::from_str(value).ok())
                }
                UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed)) => {
                    if !program_ids
                        .iter()
                        .any(|program_id| parsed.program_id == *program_id)
                    {
                        return None;
                    }
                    let kind = RaydiumLaunchpad::swap_program_data_kind_from_parsed(
                        &UiParsedInstruction::Parsed(parsed.clone()),
                    )?;
                    let info = parsed.parsed.get("info")?;
                    let keys = match kind {
                        LaunchpadSwapInstructionKind::BuyExactIn
                        | LaunchpadSwapInstructionKind::BuyExactOut => [
                            "userQuoteToken",
                            "user_quote_token",
                            "user_quote_token_account",
                        ],
                        LaunchpadSwapInstructionKind::SellExactIn
                        | LaunchpadSwapInstructionKind::SellExactOut => [
                            "userBaseToken",
                            "user_base_token",
                            "user_base_token_account",
                        ],
                    };
                    for key in keys {
                        if let Some(account) = info.get(key).and_then(|value| value.as_str())
                            && let Ok(pubkey) = Pubkey::from_str(account)
                        {
                            return Some(pubkey);
                        }
                    }
                    None
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if !program_ids.contains(program) {
                        return None;
                    }
                    let kind =
                        RaydiumLaunchpad::decode_swap_instruction_kind_base58(&compiled.data)?;
                    let source_idx = *compiled
                        .accounts
                        .get(RaydiumLaunchpad::swap_user_source_index(kind))?
                        as usize;
                    let account = account_keys.get(source_idx)?;
                    Pubkey::from_str(account).ok()
                }
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
            RAYDIUM_LAUNCHPAD_ID.to_string(),
            RAYDIUM_LAUNCHPAD_DEVNET_ID.to_string(),
        ];
        let program_id_texts = [program_ids[0].as_str(), program_ids[1].as_str()];
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        let mut user_source: Option<Pubkey> = None;
        for ix in &msg.instructions {
            user_source = extract_user_source_account(ix, &program_id_texts, &account_keys);
            if user_source.is_some() {
                break;
            }
        }

        if user_source.is_none()
            && let Some(meta) = tx.transaction.meta.as_ref()
            && let OptionSerializer::Some(inner_instructions) = &meta.inner_instructions
        {
            for inner in inner_instructions {
                for ix in &inner.instructions {
                    user_source = extract_user_source_account(ix, &program_id_texts, &account_keys);
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

    pub async fn fetch_state(&self, pool: &Pubkey) -> anyhow::Result<RaydiumLaunchpadPoolState> {
        let account = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("raydium launchpad pool account not found"))?;
        anyhow::ensure!(
            account.owner == RAYDIUM_LAUNCHPAD_ID || account.owner == RAYDIUM_LAUNCHPAD_DEVNET_ID,
            "raydium launchpad pool owner {} is not a supported launchpad program id",
            account.owner
        );
        Self::decode_pool_state_account_data(&account.data)
    }

    pub async fn fetch_global_config(
        &self,
        global_config: &Pubkey,
    ) -> anyhow::Result<RaydiumLaunchpadGlobalConfigState> {
        let account = self
            .sol
            .rpc_client
            .get_account_with_commitment(global_config, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!(
                "raydium launchpad global config account not found"
            ))?;
        anyhow::ensure!(
            account.owner == RAYDIUM_LAUNCHPAD_ID || account.owner == RAYDIUM_LAUNCHPAD_DEVNET_ID,
            "raydium launchpad global config owner {} is not a supported launchpad program id",
            account.owner
        );
        Self::decode_global_config_account_data(&account.data)
    }

    pub async fn fetch_platform_config(
        &self,
        platform_config: &Pubkey,
    ) -> anyhow::Result<RaydiumLaunchpadPlatformConfigState> {
        let account = self
            .sol
            .rpc_client
            .get_account_with_commitment(platform_config, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!(
                "raydium launchpad platform config account not found"
            ))?;
        anyhow::ensure!(
            account.owner == RAYDIUM_LAUNCHPAD_ID || account.owner == RAYDIUM_LAUNCHPAD_DEVNET_ID,
            "raydium launchpad platform config owner {} is not a supported launchpad program id",
            account.owner
        );
        Self::decode_platform_config_account_data(&account.data)
    }

    pub async fn fetch_wsol_liquidity_raw(
        &self,
        state: &RaydiumLaunchpadPoolState,
    ) -> anyhow::Result<u64> {
        if state.quote_mint == WSOL_MINT {
            return Ok(state.real_quote);
        }
        if state.base_mint == WSOL_MINT {
            return Ok(state.real_base);
        }
        anyhow::bail!("raydium launchpad pool quote mint is not WSOL")
    }

    pub async fn fetch_price(
        &self,
        pool: &Pubkey,
    ) -> anyhow::Result<(RaydiumLaunchpadPoolState, f64)> {
        let pool_account = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("raydium launchpad pool account not found"))?;
        anyhow::ensure!(
            pool_account.owner == RAYDIUM_LAUNCHPAD_ID
                || pool_account.owner == RAYDIUM_LAUNCHPAD_DEVNET_ID,
            "raydium launchpad pool owner {} is not a supported launchpad program id",
            pool_account.owner
        );
        let program_id = pool_account.owner;
        let state =
            Self::decode_pool_state_account_data(&pool_account.data).with_context(|| {
                format!("failed to decode raydium launchpad pool state for {}", pool)
            })?;

        let global_acc = self
            .sol
            .rpc_client
            .get_account_with_commitment(&state.global_config, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!(
                "raydium launchpad global config account not found"
            ))?;
        anyhow::ensure!(
            global_acc.owner == program_id,
            "raydium launchpad global config owner {} does not match program {}",
            global_acc.owner,
            program_id
        );
        let global_config = Self::decode_global_config_account_data(&global_acc.data)?;
        let price = Self::sol_price_from_pool_state(&state, global_config.curve_type)?;
        Ok((state, price))
    }

    pub async fn get_mint_from_pool(&self, pool: &Pubkey) -> anyhow::Result<Pubkey> {
        let state = self.fetch_state(pool).await?;
        if state.base_mint == WSOL_MINT && state.quote_mint != WSOL_MINT {
            return Ok(state.quote_mint);
        }
        if state.quote_mint == WSOL_MINT && state.base_mint != WSOL_MINT {
            return Ok(state.base_mint);
        }
        Ok(state.base_mint)
    }

    pub async fn find_pools_by_mint(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        let program_ids = [RAYDIUM_LAUNCHPAD_ID, RAYDIUM_LAUNCHPAD_DEVNET_ID];
        if let Some(quote) = quote_mint {
            let cfg_0 = RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::DataSize(LAUNCHPAD_POOL_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        0,
                        POOL_STATE_DISCRIM.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        LAUNCHPAD_POOL_BASE_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        LAUNCHPAD_POOL_QUOTE_MINT_OFFSET,
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
                    RpcFilterType::DataSize(LAUNCHPAD_POOL_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        0,
                        POOL_STATE_DISCRIM.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        LAUNCHPAD_POOL_BASE_MINT_OFFSET,
                        quote.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        LAUNCHPAD_POOL_QUOTE_MINT_OFFSET,
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

            let mut out = BTreeSet::new();
            for program_id in program_ids {
                let pools_0 = self
                    .sol
                    .rpc_client
                    .get_program_ui_accounts_with_config(&program_id, cfg_0.clone())
                    .await?;
                let pools_1 = self
                    .sol
                    .rpc_client
                    .get_program_ui_accounts_with_config(&program_id, cfg_1.clone())
                    .await?;
                for (pool, _) in pools_0.into_iter().chain(pools_1.into_iter()) {
                    out.insert(pool);
                }
            }
            return Ok(out.into_iter().collect());
        }

        let cfg_base = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(LAUNCHPAD_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, POOL_STATE_DISCRIM.as_ref())),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    LAUNCHPAD_POOL_BASE_MINT_OFFSET,
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
                RpcFilterType::DataSize(LAUNCHPAD_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, POOL_STATE_DISCRIM.as_ref())),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    LAUNCHPAD_POOL_QUOTE_MINT_OFFSET,
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

        let mut out = BTreeSet::new();
        for program_id in program_ids {
            let pools_base = self
                .sol
                .rpc_client
                .get_program_ui_accounts_with_config(&program_id, cfg_base.clone())
                .await?;
            let pools_quote = self
                .sol
                .rpc_client
                .get_program_ui_accounts_with_config(&program_id, cfg_quote.clone())
                .await?;
            for (pool, _) in pools_base.into_iter().chain(pools_quote.into_iter()) {
                out.insert(pool);
            }
        }

        Ok(out.into_iter().collect())
    }

    pub async fn find_pools_by_creator(&self, creator: &Pubkey) -> anyhow::Result<Vec<Pubkey>> {
        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(LAUNCHPAD_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    LAUNCHPAD_POOL_CREATOR_OFFSET,
                    creator.as_ref(),
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

        let program_id = match self.sol.cluster {
            crate::core::cluster::SolanaCluster::Devnet => RAYDIUM_LAUNCHPAD_DEVNET_ID,
            _ => RAYDIUM_LAUNCHPAD_ID,
        };
        let accounts = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&program_id, cfg)
            .await?;

        Ok(accounts.into_iter().map(|(pool, _)| pool).collect())
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
            if state.status != LAUNCHPAD_POOL_STATUS_FUND {
                continue;
            }
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

    pub async fn buy(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        _price: f64,
        use_idempotent: Option<bool>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.buy_with_priority_fee_override(
            mint,
            pool,
            _creator,
            sol_amount_in,
            slippage,
            _price,
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
        _price: f64,
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
            _price,
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
        _price: f64,
        use_idempotent: Option<bool>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.buy_for_user_with_priority_fee_override(
            buyer,
            mint,
            pool,
            _creator,
            sol_amount_in,
            slippage,
            _price,
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
        _price: f64,
        use_idempotent: Option<bool>,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        anyhow::ensure!(
            sol_amount_in > 0.0,
            "raydium launchpad buy amount must be > 0"
        );
        anyhow::ensure!(
            *mint != WSOL_MINT,
            "raydium launchpad buy mint must not be WSOL"
        );

        let buyer = *buyer;
        let pool_account = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("raydium launchpad pool account not found"))?;
        let program_id = pool_account.owner;
        anyhow::ensure!(
            program_id == RAYDIUM_LAUNCHPAD_ID || program_id == RAYDIUM_LAUNCHPAD_DEVNET_ID,
            "raydium launchpad pool owner {} is not a supported launchpad program id",
            program_id
        );
        let state =
            Self::decode_pool_state_account_data(&pool_account.data).with_context(|| {
                format!("failed to decode raydium launchpad pool state for {}", pool)
            })?;
        Self::validate_pool_contains_mint(&state, mint)?;
        Self::validate_launchpad_swap_pair(&state, mint)?;
        Self::validate_swap_enabled(&state)?;

        let (global_config, platform_config) = self.fetch_pool_configs(&program_id, &state).await?;

        let output_program = Self::token_program_for_mint(&state, mint)?;
        let input_program = Self::token_program_for_mint(&state, &WSOL_MINT)?;
        anyhow::ensure!(
            input_program == TOKEN_PROGRAM_ID,
            "raydium launchpad WSOL quote program must be token-program"
        );

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
        let amount_in_quote = (sol_amount_in * 1e9).round() as u64;
        anyhow::ensure!(
            amount_in_quote > 0,
            "raydium launchpad buy amount is too small"
        );

        let expected_out = Self::quote_buy_amount_out_raw(
            &state,
            &global_config,
            &platform_config,
            amount_in_quote,
            DEFAULT_SHARE_FEE_RATE,
        )?;
        let min_amount_out = Self::min_output_after_slippage(expected_out, slippage_pct);
        anyhow::ensure!(
            min_amount_out > 0,
            "raydium launchpad computed minimum output is zero"
        );

        ixs.push(system_instruction_if::transfer(
            &buyer,
            &input_ata,
            amount_in_quote,
        ));
        ixs.push(sync_native(&input_program, &input_ata)?);

        let mut accounts = Self::build_swap_accounts(
            &program_id,
            &state,
            pool,
            &buyer,
            output_ata,
            input_ata,
            DEFAULT_SHARE_FEE_RATE,
            None,
        )?;

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
            .context("failed to resolve priority fee for raydium launchpad buy")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_buy_exact_in_instruction_data(
            amount_in_quote,
            min_amount_out,
            DEFAULT_SHARE_FEE_RATE,
        );
        ixs.push(Instruction {
            program_id,
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
        _price: f64,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.sell_with_priority_fee_override(mint, pool, _creator, sell_pct, slippage, _price, None)
            .await
    }

    pub async fn sell_with_priority_fee_override(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        _creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        _price: f64,
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
            _price,
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
        _price: f64,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.sell_for_user_with_priority_fee_override(
            buyer, mint, pool, _creator, sell_pct, slippage, _price, None,
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
        _price: f64,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        anyhow::ensure!(
            *mint != WSOL_MINT,
            "raydium launchpad sell mint must not be WSOL"
        );

        let buyer = *buyer;
        let pool_account = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("raydium launchpad pool account not found"))?;
        let program_id = pool_account.owner;
        anyhow::ensure!(
            program_id == RAYDIUM_LAUNCHPAD_ID || program_id == RAYDIUM_LAUNCHPAD_DEVNET_ID,
            "raydium launchpad pool owner {} is not a supported launchpad program id",
            program_id
        );
        let state =
            Self::decode_pool_state_account_data(&pool_account.data).with_context(|| {
                format!("failed to decode raydium launchpad pool state for {}", pool)
            })?;
        Self::validate_pool_contains_mint(&state, mint)?;
        Self::validate_launchpad_swap_pair(&state, mint)?;
        Self::validate_swap_enabled(&state)?;
        let sell_pct = sell_pct.clamp(1, 100);

        let (global_config, platform_config) = self.fetch_pool_configs(&program_id, &state).await?;

        let input_program = Self::token_program_for_mint(&state, mint)?;
        let output_program = Self::token_program_for_mint(&state, &WSOL_MINT)?;
        anyhow::ensure!(
            output_program == TOKEN_PROGRAM_ID,
            "raydium launchpad WSOL quote program must be token-program"
        );

        let input_ata = Self::ata_for(&buyer, mint, &input_program);
        let output_ata = Self::ata_for(&buyer, &WSOL_MINT, &output_program);

        let token_balance_raw = self
            .user_token_balance_raw(&buyer, mint, &input_program)
            .await
            .context("failed to fetch token balance for raydium launchpad sell")?;
        anyhow::ensure!(
            token_balance_raw > 0,
            "no token balance for raydium launchpad sell"
        );

        let amount_in = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            amount_in > 0,
            "raydium launchpad sell amount is too small for requested percentage"
        );

        let slippage_pct = Self::normalize_slippage(slippage);
        let expected_out = Self::quote_sell_amount_out_raw(
            &state,
            &global_config,
            &platform_config,
            amount_in,
            DEFAULT_SHARE_FEE_RATE,
        )?;
        let min_amount_out = Self::min_output_after_slippage(expected_out, slippage_pct);
        anyhow::ensure!(
            min_amount_out > 0,
            "raydium launchpad computed minimum sell output is zero"
        );

        let mut ixs = vec![create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &output_program,
        )];

        let mut accounts = Self::build_swap_accounts(
            &program_id,
            &state,
            pool,
            &buyer,
            input_ata,
            output_ata,
            DEFAULT_SHARE_FEE_RATE,
            None,
        )?;

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
            .context("failed to resolve priority fee for raydium launchpad sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_sell_exact_in_instruction_data(
            amount_in,
            min_amount_out,
            DEFAULT_SHARE_FEE_RATE,
        );
        ixs.push(Instruction {
            program_id,
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
    use solana_program::hash::hash;

    fn anchor_discriminator(namespace: &str, name: &str) -> [u8; 8] {
        let preimage = format!("{namespace}:{name}");
        let digest = hash(preimage.as_bytes()).to_bytes();
        let mut out = [0u8; 8];
        out.copy_from_slice(&digest[..8]);
        out
    }

    fn encode_fixture_event(discriminator: &[u8; 8], event_payload: &[u8]) -> String {
        let mut payload = Vec::new();
        payload.extend_from_slice(discriminator);
        payload.extend_from_slice(event_payload);
        B64.encode(payload)
    }

    fn encode_trade_event_payload(event: &RaydiumLaunchpadTradeEvent) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(event.pool_state.as_ref());
        payload.extend_from_slice(&event.total_base_sell.to_le_bytes());
        payload.extend_from_slice(&event.virtual_base.to_le_bytes());
        payload.extend_from_slice(&event.virtual_quote.to_le_bytes());
        payload.extend_from_slice(&event.real_base_before.to_le_bytes());
        payload.extend_from_slice(&event.real_quote_before.to_le_bytes());
        payload.extend_from_slice(&event.real_base_after.to_le_bytes());
        payload.extend_from_slice(&event.real_quote_after.to_le_bytes());
        payload.extend_from_slice(&event.amount_in.to_le_bytes());
        payload.extend_from_slice(&event.amount_out.to_le_bytes());
        payload.extend_from_slice(&event.protocol_fee.to_le_bytes());
        payload.extend_from_slice(&event.platform_fee.to_le_bytes());
        payload.extend_from_slice(&event.creator_fee.to_le_bytes());
        payload.extend_from_slice(&event.share_fee.to_le_bytes());
        payload.push(event.trade_direction);
        payload.push(event.pool_status);
        payload.push(if event.exact_in { 1 } else { 0 });
        payload
    }

    fn synthetic_pool_state_account_bytes() -> Vec<u8> {
        let mut data = vec![0u8; LAUNCHPAD_POOL_ACCOUNT_LEN];
        data[..8].copy_from_slice(&POOL_STATE_DISCRIM);

        let global_config = Pubkey::new_unique();
        let platform_config = Pubkey::new_unique();
        let base_mint = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let creator = Pubkey::new_unique();

        data[LAUNCHPAD_POOL_EPOCH_OFFSET..LAUNCHPAD_POOL_EPOCH_OFFSET + 8]
            .copy_from_slice(&42u64.to_le_bytes());
        data[LAUNCHPAD_POOL_AUTH_BUMP_OFFSET] = 254;
        data[LAUNCHPAD_POOL_STATUS_OFFSET] = LAUNCHPAD_POOL_STATUS_FUND;
        data[LAUNCHPAD_POOL_BASE_DECIMALS_OFFSET] = 6;
        data[LAUNCHPAD_POOL_QUOTE_DECIMALS_OFFSET] = 9;
        data[LAUNCHPAD_POOL_MIGRATE_TYPE_OFFSET] = 1;
        data[LAUNCHPAD_POOL_SUPPLY_OFFSET..LAUNCHPAD_POOL_SUPPLY_OFFSET + 8]
            .copy_from_slice(&1_000_000_000u64.to_le_bytes());
        data[LAUNCHPAD_POOL_TOTAL_BASE_SELL_OFFSET..LAUNCHPAD_POOL_TOTAL_BASE_SELL_OFFSET + 8]
            .copy_from_slice(&800_000_000u64.to_le_bytes());
        data[LAUNCHPAD_POOL_VIRTUAL_BASE_OFFSET..LAUNCHPAD_POOL_VIRTUAL_BASE_OFFSET + 8]
            .copy_from_slice(&1_500_000_000u64.to_le_bytes());
        data[LAUNCHPAD_POOL_VIRTUAL_QUOTE_OFFSET..LAUNCHPAD_POOL_VIRTUAL_QUOTE_OFFSET + 8]
            .copy_from_slice(&500_000_000u64.to_le_bytes());
        data[LAUNCHPAD_POOL_REAL_BASE_OFFSET..LAUNCHPAD_POOL_REAL_BASE_OFFSET + 8]
            .copy_from_slice(&100_000_000u64.to_le_bytes());
        data[LAUNCHPAD_POOL_REAL_QUOTE_OFFSET..LAUNCHPAD_POOL_REAL_QUOTE_OFFSET + 8]
            .copy_from_slice(&50_000_000u64.to_le_bytes());
        data[LAUNCHPAD_POOL_TOTAL_QUOTE_FUND_RAISING_OFFSET
            ..LAUNCHPAD_POOL_TOTAL_QUOTE_FUND_RAISING_OFFSET + 8]
            .copy_from_slice(&100_000_000u64.to_le_bytes());
        data[LAUNCHPAD_POOL_QUOTE_PROTOCOL_FEE_OFFSET
            ..LAUNCHPAD_POOL_QUOTE_PROTOCOL_FEE_OFFSET + 8]
            .copy_from_slice(&100u64.to_le_bytes());
        data[LAUNCHPAD_POOL_PLATFORM_FEE_OFFSET..LAUNCHPAD_POOL_PLATFORM_FEE_OFFSET + 8]
            .copy_from_slice(&200u64.to_le_bytes());
        data[LAUNCHPAD_POOL_MIGRATE_FEE_OFFSET..LAUNCHPAD_POOL_MIGRATE_FEE_OFFSET + 8]
            .copy_from_slice(&300u64.to_le_bytes());
        data[LAUNCHPAD_POOL_VESTING_TOTAL_LOCKED_AMOUNT_OFFSET
            ..LAUNCHPAD_POOL_VESTING_TOTAL_LOCKED_AMOUNT_OFFSET + 8]
            .copy_from_slice(&400u64.to_le_bytes());
        data[LAUNCHPAD_POOL_VESTING_CLIFF_PERIOD_OFFSET
            ..LAUNCHPAD_POOL_VESTING_CLIFF_PERIOD_OFFSET + 8]
            .copy_from_slice(&500u64.to_le_bytes());
        data[LAUNCHPAD_POOL_VESTING_UNLOCK_PERIOD_OFFSET
            ..LAUNCHPAD_POOL_VESTING_UNLOCK_PERIOD_OFFSET + 8]
            .copy_from_slice(&600u64.to_le_bytes());
        data[LAUNCHPAD_POOL_VESTING_START_TIME_OFFSET
            ..LAUNCHPAD_POOL_VESTING_START_TIME_OFFSET + 8]
            .copy_from_slice(&700u64.to_le_bytes());
        data[LAUNCHPAD_POOL_VESTING_ALLOCATED_SHARE_AMOUNT_OFFSET
            ..LAUNCHPAD_POOL_VESTING_ALLOCATED_SHARE_AMOUNT_OFFSET + 8]
            .copy_from_slice(&800u64.to_le_bytes());

        data[LAUNCHPAD_POOL_GLOBAL_CONFIG_OFFSET..LAUNCHPAD_POOL_GLOBAL_CONFIG_OFFSET + 32]
            .copy_from_slice(global_config.as_ref());
        data[LAUNCHPAD_POOL_PLATFORM_CONFIG_OFFSET..LAUNCHPAD_POOL_PLATFORM_CONFIG_OFFSET + 32]
            .copy_from_slice(platform_config.as_ref());
        data[LAUNCHPAD_POOL_BASE_MINT_OFFSET..LAUNCHPAD_POOL_BASE_MINT_OFFSET + 32]
            .copy_from_slice(base_mint.as_ref());
        data[LAUNCHPAD_POOL_QUOTE_MINT_OFFSET..LAUNCHPAD_POOL_QUOTE_MINT_OFFSET + 32]
            .copy_from_slice(WSOL_MINT.as_ref());
        data[LAUNCHPAD_POOL_BASE_VAULT_OFFSET..LAUNCHPAD_POOL_BASE_VAULT_OFFSET + 32]
            .copy_from_slice(base_vault.as_ref());
        data[LAUNCHPAD_POOL_QUOTE_VAULT_OFFSET..LAUNCHPAD_POOL_QUOTE_VAULT_OFFSET + 32]
            .copy_from_slice(quote_vault.as_ref());
        data[LAUNCHPAD_POOL_CREATOR_OFFSET..LAUNCHPAD_POOL_CREATOR_OFFSET + 32]
            .copy_from_slice(creator.as_ref());
        data[LAUNCHPAD_POOL_TOKEN_PROGRAM_FLAG_OFFSET] = 0;
        data[LAUNCHPAD_POOL_AMM_FEE_ON_OFFSET] = 1;

        data
    }

    fn synthetic_global_config_account_bytes() -> Vec<u8> {
        let mut data = vec![0u8; LAUNCHPAD_GLOBAL_CONFIG_ACCOUNT_LEN];
        data[..8].copy_from_slice(&GLOBAL_CONFIG_DISCRIM);

        data[LAUNCHPAD_GLOBAL_CONFIG_CURVE_TYPE_OFFSET] = 0;
        data[LAUNCHPAD_GLOBAL_CONFIG_TRADE_FEE_RATE_OFFSET
            ..LAUNCHPAD_GLOBAL_CONFIG_TRADE_FEE_RATE_OFFSET + 8]
            .copy_from_slice(&2_500u64.to_le_bytes());
        data[LAUNCHPAD_GLOBAL_CONFIG_MAX_SHARE_FEE_RATE_OFFSET
            ..LAUNCHPAD_GLOBAL_CONFIG_MAX_SHARE_FEE_RATE_OFFSET + 8]
            .copy_from_slice(&100_000u64.to_le_bytes());
        data[LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_SUPPLY_OFFSET
            ..LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_SUPPLY_OFFSET + 8]
            .copy_from_slice(&10_000_000u64.to_le_bytes());
        data[LAUNCHPAD_GLOBAL_CONFIG_MAX_LOCK_RATE_OFFSET
            ..LAUNCHPAD_GLOBAL_CONFIG_MAX_LOCK_RATE_OFFSET + 8]
            .copy_from_slice(&300_000u64.to_le_bytes());
        data[LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_SELL_RATE_OFFSET
            ..LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_SELL_RATE_OFFSET + 8]
            .copy_from_slice(&200_000u64.to_le_bytes());
        data[LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_MIGRATE_RATE_OFFSET
            ..LAUNCHPAD_GLOBAL_CONFIG_MIN_BASE_MIGRATE_RATE_OFFSET + 8]
            .copy_from_slice(&200_000u64.to_le_bytes());
        data[LAUNCHPAD_GLOBAL_CONFIG_MIN_QUOTE_FUND_RAISING_OFFSET
            ..LAUNCHPAD_GLOBAL_CONFIG_MIN_QUOTE_FUND_RAISING_OFFSET + 8]
            .copy_from_slice(&30_000_000_000u64.to_le_bytes());
        data[LAUNCHPAD_GLOBAL_CONFIG_QUOTE_MINT_OFFSET
            ..LAUNCHPAD_GLOBAL_CONFIG_QUOTE_MINT_OFFSET + 32]
            .copy_from_slice(WSOL_MINT.as_ref());

        data
    }

    fn synthetic_platform_config_account_bytes() -> Vec<u8> {
        let mut data = vec![0u8; LAUNCHPAD_PLATFORM_CONFIG_ACCOUNT_MIN_LEN];
        data[..8].copy_from_slice(&PLATFORM_CONFIG_DISCRIM);

        let platform_fee_wallet = Pubkey::new_unique();
        data[LAUNCHPAD_PLATFORM_CONFIG_PLATFORM_FEE_WALLET_OFFSET
            ..LAUNCHPAD_PLATFORM_CONFIG_PLATFORM_FEE_WALLET_OFFSET + 32]
            .copy_from_slice(platform_fee_wallet.as_ref());
        data[LAUNCHPAD_PLATFORM_CONFIG_FEE_RATE_OFFSET
            ..LAUNCHPAD_PLATFORM_CONFIG_FEE_RATE_OFFSET + 8]
            .copy_from_slice(&1_000u64.to_le_bytes());
        data[LAUNCHPAD_PLATFORM_CONFIG_CREATOR_FEE_RATE_OFFSET
            ..LAUNCHPAD_PLATFORM_CONFIG_CREATOR_FEE_RATE_OFFSET + 8]
            .copy_from_slice(&500u64.to_le_bytes());
        data
    }

    #[test]
    fn test_raydium_launchpad_discriminators_match_anchor_layout() {
        assert_eq!(
            BUY_EXACT_IN_IX_DISCRIM,
            anchor_discriminator("global", "buy_exact_in")
        );
        assert_eq!(
            BUY_EXACT_OUT_IX_DISCRIM,
            anchor_discriminator("global", "buy_exact_out")
        );
        assert_eq!(
            SELL_EXACT_IN_IX_DISCRIM,
            anchor_discriminator("global", "sell_exact_in")
        );
        assert_eq!(
            SELL_EXACT_OUT_IX_DISCRIM,
            anchor_discriminator("global", "sell_exact_out")
        );
        assert_eq!(
            POOL_STATE_DISCRIM,
            anchor_discriminator("account", "PoolState")
        );
        assert_eq!(
            GLOBAL_CONFIG_DISCRIM,
            anchor_discriminator("account", "GlobalConfig")
        );
        assert_eq!(
            PLATFORM_CONFIG_DISCRIM,
            anchor_discriminator("account", "PlatformConfig")
        );
        assert_eq!(
            TRADE_EVENT_DISCRIM,
            anchor_discriminator("event", "TradeEvent")
        );
        assert_eq!(
            POOL_CREATE_EVENT_DISCRIM,
            anchor_discriminator("event", "PoolCreateEvent")
        );
    }

    #[test]
    fn test_raydium_launchpad_program_constants() {
        assert_eq!(
            RAYDIUM_LAUNCHPAD_ID,
            Pubkey::from_str("LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj").unwrap()
        );
        assert_eq!(
            RAYDIUM_LAUNCHPAD_AUTH,
            Pubkey::from_str("WLHv2UAZm6z4KyaaELi5pjdbJh6RESMva1Rnn8pJVVh").unwrap()
        );
        assert_eq!(LAUNCHPAD_POOL_ACCOUNT_LEN, 429);
        assert_eq!(LAUNCHPAD_POOL_BASE_MINT_OFFSET, 205);
        assert_eq!(LAUNCHPAD_POOL_QUOTE_MINT_OFFSET, 237);
    }

    #[test]
    fn test_raydium_launchpad_normalize_slippage() {
        assert_eq!(RaydiumLaunchpad::normalize_slippage(15.0), 0.15);
        assert_eq!(RaydiumLaunchpad::normalize_slippage(0.2), 0.2);
        assert_eq!(RaydiumLaunchpad::normalize_slippage(0.0), 0.01);
        assert_eq!(RaydiumLaunchpad::normalize_slippage(120.0), 0.99);
    }

    #[test]
    fn test_raydium_launchpad_encode_instruction_data_layout() {
        let buy = RaydiumLaunchpad::encode_buy_exact_in_instruction_data(123, 45, 0);
        assert_eq!(buy.len(), 32);
        assert_eq!(&buy[..8], &BUY_EXACT_IN_IX_DISCRIM);
        assert_eq!(u64::from_le_bytes(buy[8..16].try_into().unwrap()), 123);
        assert_eq!(u64::from_le_bytes(buy[16..24].try_into().unwrap()), 45);
        assert_eq!(u64::from_le_bytes(buy[24..32].try_into().unwrap()), 0);

        let sell = RaydiumLaunchpad::encode_sell_exact_in_instruction_data(77, 66, 5);
        assert_eq!(sell.len(), 32);
        assert_eq!(&sell[..8], &SELL_EXACT_IN_IX_DISCRIM);
        assert_eq!(u64::from_le_bytes(sell[8..16].try_into().unwrap()), 77);
        assert_eq!(u64::from_le_bytes(sell[16..24].try_into().unwrap()), 66);
        assert_eq!(u64::from_le_bytes(sell[24..32].try_into().unwrap()), 5);
    }

    #[test]
    fn test_raydium_launchpad_decode_pool_state_layout() {
        let data = synthetic_pool_state_account_bytes();
        let state = RaydiumLaunchpad::decode_pool_state_account_data(&data).unwrap();
        assert_eq!(state.epoch, 42);
        assert_eq!(state.auth_bump, 254);
        assert_eq!(state.status, LAUNCHPAD_POOL_STATUS_FUND);
        assert_eq!(state.base_decimals, 6);
        assert_eq!(state.quote_decimals, 9);
        assert_eq!(state.supply, 1_000_000_000);
        assert_eq!(state.total_base_sell, 800_000_000);
        assert_eq!(state.real_quote, 50_000_000);
        assert_eq!(state.quote_mint, WSOL_MINT);
    }

    #[test]
    fn test_raydium_launchpad_decode_global_and_platform_layouts() {
        let global = RaydiumLaunchpad::decode_global_config_account_data(
            &synthetic_global_config_account_bytes(),
        )
        .unwrap();
        assert_eq!(global.curve_type, 0);
        assert_eq!(global.trade_fee_rate, 2_500);
        assert_eq!(global.max_share_fee_rate, 100_000);
        assert_eq!(global.min_base_supply, 10_000_000);
        assert_eq!(global.max_lock_rate, 300_000);
        assert_eq!(global.min_base_sell_rate, 200_000);
        assert_eq!(global.min_base_migrate_rate, 200_000);
        assert_eq!(global.min_quote_fund_raising, 30_000_000_000);
        assert_eq!(global.quote_mint, WSOL_MINT);

        let platform = RaydiumLaunchpad::decode_platform_config_account_data(
            &synthetic_platform_config_account_bytes(),
        )
        .unwrap();
        assert_eq!(platform.fee_rate, 1_000);
        assert_eq!(platform.creator_fee_rate, 500);
        assert_ne!(platform.platform_fee_wallet, Pubkey::default());
    }

    #[test]
    fn test_raydium_launchpad_decode_platform_curve_params_layout() {
        let global_a = Pubkey::new_from_array([1u8; 32]);
        let global_b = Pubkey::new_from_array([2u8; 32]);

        let entry_a = RaydiumLaunchpadPlatformCurveParam {
            epoch: 111,
            index: 7,
            global_config: global_a,
            bonding_curve_param: RaydiumLaunchpadBondingCurveParam {
                migrate_type: 1,
                amm_fee_on: 0,
                supply: 1_000,
                total_base_sell: 2_000,
                total_quote_fund_raising: 3_000,
                total_locked_amount: 4_000,
                cliff_period: 5_000,
                unlock_period: 6_000,
            },
        };
        let entry_b = RaydiumLaunchpadPlatformCurveParam {
            epoch: 222,
            index: 9,
            global_config: global_b,
            bonding_curve_param: RaydiumLaunchpadBondingCurveParam {
                migrate_type: 2,
                amm_fee_on: 1,
                supply: 9_999,
                total_base_sell: 8_888,
                total_quote_fund_raising: 7_777,
                total_locked_amount: 6_666,
                cliff_period: 5_555,
                unlock_period: 4_444,
            },
        };

        let mut data = vec![0u8; LAUNCHPAD_PLATFORM_CONFIG_CURVE_PARAMS_OFFSET];
        data[..8].copy_from_slice(&PLATFORM_CONFIG_DISCRIM);
        data.extend_from_slice(&(2u32).to_le_bytes());

        for entry in [&entry_a, &entry_b] {
            data.extend_from_slice(&entry.epoch.to_le_bytes());
            data.push(entry.index);
            data.extend_from_slice(entry.global_config.as_ref());
            data.push(entry.bonding_curve_param.migrate_type);
            data.push(entry.bonding_curve_param.amm_fee_on);
            data.extend_from_slice(&entry.bonding_curve_param.supply.to_le_bytes());
            data.extend_from_slice(&entry.bonding_curve_param.total_base_sell.to_le_bytes());
            data.extend_from_slice(
                &entry
                    .bonding_curve_param
                    .total_quote_fund_raising
                    .to_le_bytes(),
            );
            data.extend_from_slice(&entry.bonding_curve_param.total_locked_amount.to_le_bytes());
            data.extend_from_slice(&entry.bonding_curve_param.cliff_period.to_le_bytes());
            data.extend_from_slice(&entry.bonding_curve_param.unlock_period.to_le_bytes());

            data.resize(data.len() + (8 * 50), 0xAB);
        }

        let decoded = RaydiumLaunchpad::decode_platform_curve_params(&data).unwrap();
        assert_eq!(decoded, vec![entry_a, entry_b]);
    }

    #[test]
    fn test_raydium_launchpad_decode_mainnet_platform_config_fixture() {
        let data = B64
            .decode("oE6AAPhT5qCaAwAAAAAAAF+c003hCJPjeSL/8lEjOV5vWFk4s5VwDiWJMuJCigIX1vf8PodPcr5cE/EkPd/kBxZkkmnr21jvFJW1XKsrbTpAQg8AAAAAAAAAAAAAAAAAAAAAAAAAAACIEwAAAAAAAFphcHp5AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABodHRwczovL3phcHp5LmlvAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAaHR0cHM6Ly9pcGZzLmlvL2lwZnMvYmFma3JlaWNsYWVka3R6Z3F0MjR4emxla2c2ZGhrZ3loeDU0NndocnU0bXdoZHluMzJlYmthbnBrdW0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAODxkGrDXXr/nr20x291VPvcTU/b+b+o2kC9G0kcXeTliBMAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAF+c003hCJPjeSL/8lEjOV5vWFk4s5VwDiWJMuJCigIXAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEAAABIAwAAAAAAAABXGo4ByN94IPnWazxzZbjR5K+oG3hUzC73XO9YvQiGfgH/AIDGpH6NAwAAeMX7UdECAAASZcoTAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==")
            .expect("fixture base64 must decode");

        let state = RaydiumLaunchpad::decode_platform_config_account_data(&data).unwrap();
        assert_eq!(state.fee_rate, 5_000);

        let info = RaydiumLaunchpad::decode_platform_config_info(&data).unwrap();
        assert_eq!(info.name, "Zapzy");

        let curve_params = RaydiumLaunchpad::decode_platform_curve_params(&data).unwrap();
        assert_eq!(curve_params.len(), 1);
        assert_eq!(
            curve_params[0].global_config,
            Pubkey::from_str("6s1xP3hpbAfFoNtUNF8mfHsjr2Bd97JxFJRWLbL6aHuX").unwrap()
        );
        assert_eq!(curve_params[0].bonding_curve_param.migrate_type, 1);
    }

    #[test]
    fn test_raydium_launchpad_sol_price_curve_variants() {
        let mut state =
            RaydiumLaunchpad::decode_pool_state_account_data(&synthetic_pool_state_account_bytes())
                .unwrap();

        state.virtual_base = 1_000_000;
        state.real_base = 200_000;
        state.virtual_quote = 500_000;
        state.real_quote = 100_000;
        let curve_0_price = RaydiumLaunchpad::sol_price_from_pool_state(&state, 0).unwrap();
        assert!((curve_0_price - 0.00075).abs() < 1e-12);

        let curve_1_price = RaydiumLaunchpad::sol_price_from_pool_state(&state, 1).unwrap();
        assert!((curve_1_price - 0.0005).abs() < 1e-12);

        // Q64 (1 << 64) is not representable as u64; use the closest value to exercise curve type 2.
        state.virtual_base = u64::MAX;
        state.real_base = 2;
        state.virtual_quote = 0;
        state.real_quote = 0;
        let curve_2_price = RaydiumLaunchpad::sol_price_from_pool_state(&state, 2).unwrap();
        assert!((curve_2_price - 0.002).abs() < 1e-12);
    }

    #[test]
    fn test_raydium_launchpad_quote_buy_and_sell_exact_in() {
        let mut state =
            RaydiumLaunchpad::decode_pool_state_account_data(&synthetic_pool_state_account_bytes())
                .unwrap();
        state.virtual_base = 1_000_000;
        state.real_base = 100_000;
        state.virtual_quote = 2_000_000;
        state.real_quote = 500_000;
        state.total_base_sell = 900_000;

        let global = RaydiumLaunchpadGlobalConfigState {
            curve_type: 0,
            trade_fee_rate: 0,
            max_share_fee_rate: 100_000,
            min_base_supply: 10_000_000,
            max_lock_rate: 300_000,
            min_base_sell_rate: 200_000,
            min_base_migrate_rate: 200_000,
            min_quote_fund_raising: 1,
            quote_mint: WSOL_MINT,
        };
        let platform = RaydiumLaunchpadPlatformConfigState {
            platform_fee_wallet: Pubkey::new_unique(),
            fee_rate: 0,
            creator_fee_rate: 0,
        };

        let buy_out =
            RaydiumLaunchpad::quote_buy_amount_out_raw(&state, &global, &platform, 100_000, 0)
                .unwrap();
        assert_eq!(buy_out, 34_615);

        let sell_out =
            RaydiumLaunchpad::quote_sell_amount_out_raw(&state, &global, &platform, 50_000, 0)
                .unwrap();
        assert_eq!(sell_out, 131_578);
    }

    #[test]
    fn test_raydium_launchpad_parse_logs_ignores_invalid_payloads() {
        let logs = vec![
            "Program log: hello".to_string(),
            "Program data: not-base64".to_string(),
            format!("Program data: {}", B64.encode([1u8, 2, 3, 4])),
        ];
        let events = RaydiumLaunchpad::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_raydium_launchpad_parse_logs_unknown_event() {
        let payload = [1u8, 1, 1, 1, 1, 1, 1, 1, 99];
        let logs = vec![format!("Program data: {}", B64.encode(payload))];
        let events = RaydiumLaunchpad::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], RaydiumLaunchpadEvent::Unknown));
    }

    #[test]
    fn test_raydium_launchpad_parse_logs_trade_fixture_decodes() {
        let event = RaydiumLaunchpadTradeEvent {
            pool_state: Pubkey::new_unique(),
            total_base_sell: 111,
            virtual_base: 222,
            virtual_quote: 333,
            real_base_before: 444,
            real_quote_before: 555,
            real_base_after: 666,
            real_quote_after: 777,
            amount_in: 888,
            amount_out: 999,
            protocol_fee: 10,
            platform_fee: 11,
            creator_fee: 12,
            share_fee: 13,
            trade_direction: LAUNCHPAD_TRADE_DIRECTION_BUY,
            pool_status: LAUNCHPAD_POOL_STATUS_FUND,
            exact_in: true,
        };

        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&TRADE_EVENT_DISCRIM, &encode_trade_event_payload(&event))
        )];
        let events = RaydiumLaunchpad::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumLaunchpadEvent::Trade(Some(parsed)) => {
                assert_eq!(parsed.pool_state, event.pool_state);
                assert_eq!(parsed.amount_in, event.amount_in);
                assert_eq!(parsed.amount_out, event.amount_out);
                assert_eq!(parsed.trade_direction, event.trade_direction);
                assert!(parsed.exact_in);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_raydium_launchpad_extract_pool_from_inner_instruction_fixture() {
        let payer = Pubkey::new_unique();
        let authority = RaydiumLaunchpad::derive_authority_pda(&RAYDIUM_LAUNCHPAD_ID);
        let global_config = Pubkey::new_unique();
        let platform_config = Pubkey::new_unique();
        let pool = Pubkey::new_unique();
        let user_base = Pubkey::new_unique();
        let user_quote = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();
        let base_mint = Pubkey::new_unique();
        let quote_mint = WSOL_MINT;
        let base_program = TOKEN_PROGRAM_ID;
        let quote_program = TOKEN_PROGRAM_ID;
        let event_authority = RaydiumLaunchpad::derive_event_authority_pda(&RAYDIUM_LAUNCHPAD_ID);
        let share_receiver = Pubkey::new_unique();
        let platform_fee_vault = Pubkey::new_unique();
        let creator_fee_vault = Pubkey::new_unique();

        let mut ix_data = Vec::new();
        ix_data.extend_from_slice(&BUY_EXACT_IN_IX_DISCRIM);
        ix_data.extend_from_slice(&1_000u64.to_le_bytes());
        ix_data.extend_from_slice(&2_000u64.to_le_bytes());
        ix_data.extend_from_slice(&0u64.to_le_bytes());
        let ix_data_b58 = bs58::encode(ix_data).into_string();

        let tx_json = json!({
            "slot": 1,
            "transaction": {
                "signatures": ["1111111111111111111111111111111111111111111111111111111111111111"],
                "message": {
                    "accountKeys": [
                        {
                            "pubkey": payer.to_string(),
                            "writable": true,
                            "signer": true
                        },
                        {
                            "pubkey": authority.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": global_config.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": platform_config.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": pool.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": user_base.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": user_quote.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": base_vault.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": quote_vault.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": base_mint.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": quote_mint.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": base_program.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": quote_program.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": event_authority.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": RAYDIUM_LAUNCHPAD_ID.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": share_receiver.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": SYSTEM_PROGRAM.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": platform_fee_vault.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": creator_fee_vault.to_string(),
                            "writable": true,
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
                                "programId": RAYDIUM_LAUNCHPAD_ID.to_string(),
                                "accounts": [
                                    payer.to_string(),
                                    authority.to_string(),
                                    global_config.to_string(),
                                    platform_config.to_string(),
                                    pool.to_string(),
                                    user_base.to_string(),
                                    user_quote.to_string(),
                                    base_vault.to_string(),
                                    quote_vault.to_string(),
                                    base_mint.to_string(),
                                    quote_mint.to_string(),
                                    base_program.to_string(),
                                    quote_program.to_string(),
                                    event_authority.to_string(),
                                    RAYDIUM_LAUNCHPAD_ID.to_string(),
                                    SYSTEM_PROGRAM.to_string(),
                                    platform_fee_vault.to_string(),
                                    creator_fee_vault.to_string()
                                ],
                                "data": ix_data_b58,
                                "stackHeight": 2
                            }
                        ]
                    }
                ]
            }
        });

        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(tx_json).unwrap();
        assert_eq!(
            RaydiumLaunchpad::extract_pool_from_transaction(&tx),
            Some(pool)
        );
    }
}
