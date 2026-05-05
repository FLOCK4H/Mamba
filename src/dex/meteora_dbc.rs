use crate::core::sol::{
    DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SolHook, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, WSOL_MINT,
};
use crate::utils::utils::decode_b64;
use crate::utils::writing::cc;
use crate::{log, warn};
use anyhow::Context;
use ruint::aliases::U256;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    sysvar::instructions as sysvar_instructions,
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
use std::io::{Cursor, Read};
use std::{collections::BTreeSet, str::FromStr, sync::Arc, time::Duration};

pub const METEORA_DBC_ID: Pubkey =
    Pubkey::from_str_const("dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN");
pub const METEORA_DBC_POOL_AUTHORITY: Pubkey =
    Pubkey::from_str_const("FhVo3mqL8PW5pH5U2CN4XE33DokiyZnUwuGpH2hmHLuM");

pub const SWAP_IX_DISCRIM: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
pub const INITIALIZE_VIRTUAL_POOL_WITH_SPL_TOKEN_IX_DISCRIM: [u8; 8] =
    [140, 85, 215, 176, 102, 54, 104, 79];
pub const INITIALIZE_VIRTUAL_POOL_WITH_TOKEN_2022_IX_DISCRIM: [u8; 8] =
    [169, 118, 51, 78, 145, 110, 220, 155];
pub const VIRTUAL_POOL_DISCRIM: [u8; 8] = [213, 224, 5, 209, 98, 69, 119, 92];
pub const POOL_CONFIG_DISCRIM: [u8; 8] = [26, 108, 14, 123, 116, 230, 129, 43];
pub const EVT_INITIALIZE_POOL_EVENT_DISCRIM: [u8; 8] = [228, 50, 246, 85, 203, 66, 134, 37];
pub const EVT_SWAP2_EVENT_DISCRIM: [u8; 8] = [189, 66, 51, 168, 38, 80, 117, 153];
pub const EVENT_CPI_IX_DISCRIM: [u8; 8] = [228, 69, 165, 46, 81, 203, 154, 29];

pub const SEARCH_FOR: &str = "Program data: ";
pub const POOL_AUTHORITY_SEED: &[u8] = b"pool_authority";
pub const EVENT_AUTHORITY_SEED: &[u8] = b"__event_authority";
pub const POOL_PREFIX: &[u8] = b"pool";
pub const TOKEN_VAULT_PREFIX: &[u8] = b"token_vault";

pub const DBC_VIRTUAL_POOL_ACCOUNT_LEN: usize = 424;
pub const DBC_POOL_CONFIG_ACCOUNT_LEN: usize = 1048;
pub const DBC_VIRTUAL_POOL_MIN_DECODE_LEN: usize = 312;
pub const DBC_POOL_CONFIG_MIN_DECODE_LEN: usize = 238;

pub const DBC_VIRTUAL_POOL_CONFIG_OFFSET: usize = 72;
pub const DBC_VIRTUAL_POOL_CREATOR_OFFSET: usize = 104;
pub const DBC_VIRTUAL_POOL_BASE_MINT_OFFSET: usize = 136;
pub const DBC_VIRTUAL_POOL_BASE_VAULT_OFFSET: usize = 168;
pub const DBC_VIRTUAL_POOL_QUOTE_VAULT_OFFSET: usize = 200;
pub const DBC_VIRTUAL_POOL_BASE_RESERVE_OFFSET: usize = 232;
pub const DBC_VIRTUAL_POOL_QUOTE_RESERVE_OFFSET: usize = 240;
pub const DBC_VIRTUAL_POOL_SQRT_PRICE_OFFSET: usize = 280;
pub const DBC_VIRTUAL_POOL_POOL_TYPE_OFFSET: usize = 304;
pub const DBC_VIRTUAL_POOL_IS_MIGRATED_OFFSET: usize = 305;
pub const DBC_VIRTUAL_POOL_MIGRATION_PROGRESS_OFFSET: usize = 308;

pub const DBC_POOL_CONFIG_QUOTE_MINT_OFFSET: usize = 8;
pub const DBC_POOL_CONFIG_COLLECT_FEE_MODE_OFFSET: usize = 232;
pub const DBC_POOL_CONFIG_MIGRATION_OPTION_OFFSET: usize = 233;
pub const DBC_POOL_CONFIG_TOKEN_DECIMAL_OFFSET: usize = 235;
pub const DBC_POOL_CONFIG_TOKEN_TYPE_OFFSET: usize = 237;
pub const DBC_POOL_CONFIG_PARTNER_VESTING_INFO_OFFSET: usize = 184;
pub const DBC_POOL_CONFIG_CREATOR_VESTING_INFO_OFFSET: usize = 200;
pub const DBC_POOL_CONFIG_PARTNER_PERMANENT_LOCKED_LIQ_PCT_OFFSET: usize = 239;
pub const DBC_POOL_CONFIG_CREATOR_PERMANENT_LOCKED_LIQ_PCT_OFFSET: usize = 241;

pub const DBC_MIN_LOCKED_LIQUIDITY_BPS: u16 = 1000; // 10%
pub const DBC_LOCKED_LIQUIDITY_CHECK_SECONDS: u64 = 86_400; // 1 day

#[derive(Debug, Clone)]
pub struct MeteoraDbcVirtualPoolState {
    pub config: Pubkey,
    pub creator: Pubkey,
    pub base_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub sqrt_price: u128,
    pub pool_type: u8,
    pub is_migrated: u8,
    pub migration_progress: u8,
}

#[derive(Debug, Clone)]
pub struct MeteoraDbcConfigState {
    pub quote_mint: Pubkey,
    pub collect_fee_mode: u8,
    pub migration_option: u8,
    pub token_decimal: u8,
    pub token_type: u8,
    pub partner_liquidity_vesting_info: DbcLiquidityVestingInfo,
    pub creator_liquidity_vesting_info: DbcLiquidityVestingInfo,
    pub partner_permanent_locked_liquidity_percentage: u8,
    pub creator_permanent_locked_liquidity_percentage: u8,
}

#[derive(Debug, Clone)]
pub struct MeteoraDbcPoolState {
    pub virtual_pool: MeteoraDbcVirtualPoolState,
    pub config: MeteoraDbcConfigState,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DbcLiquidityVestingInfo {
    pub is_initialized: u8,
    pub vesting_percentage: u8,
    pub bps_per_period: u16,
    pub number_of_periods: u16,
    pub frequency: u32,
    pub cliff_duration_from_migration_time: u32,
}

impl DbcLiquidityVestingInfo {
    pub fn decode_from(data: &[u8], offset: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(
            data.len() >= offset + 16,
            "missing liquidity vesting info bytes at offset {offset}"
        );
        Ok(Self {
            is_initialized: data[offset],
            vesting_percentage: data[offset + 1],
            bps_per_period: u16::from_le_bytes([data[offset + 4], data[offset + 5]]),
            number_of_periods: u16::from_le_bytes([data[offset + 6], data[offset + 7]]),
            frequency: u32::from_le_bytes([
                data[offset + 8],
                data[offset + 9],
                data[offset + 10],
                data[offset + 11],
            ]),
            cliff_duration_from_migration_time: u32::from_le_bytes([
                data[offset + 12],
                data[offset + 13],
                data[offset + 14],
                data[offset + 15],
            ]),
        })
    }

    fn mul_div_u128_round_down(a: u128, b: u128, denom: u128) -> anyhow::Result<u128> {
        anyhow::ensure!(denom != 0, "division by zero");
        let q: U256 = (U256::from(a) * U256::from(b)) / U256::from(denom);
        q.try_into()
            .map_err(|_| anyhow::anyhow!("mul_div overflow for u128"))
    }

    fn max_unlocked_liquidity_at_current_point(
        cliff_point: u64,
        liquidity_per_period: u128,
        cliff_unlock_liquidity: u128,
        period_frequency: u64,
        number_of_periods: u16,
        current_point: u64,
    ) -> anyhow::Result<u128> {
        if current_point < cliff_point {
            return Ok(0);
        }
        if period_frequency == 0 {
            return Ok(cliff_unlock_liquidity);
        }
        let period = current_point
            .checked_sub(cliff_point)
            .context("vesting current_point underflow")?
            .checked_div(period_frequency)
            .context("vesting period_frequency division failed")?;
        let period = period.min(number_of_periods as u64) as u128;

        cliff_unlock_liquidity
            .checked_add(
                period
                    .checked_mul(liquidity_per_period)
                    .context("vesting unlocked liquidity overflow")?,
            )
            .context("vesting unlocked liquidity overflow")
    }

    pub fn liquidity_locked_bps_at_n_seconds(&self, n_seconds: u64) -> anyhow::Result<u16> {
        const MAX_BASIS_POINT: u128 = 10_000;
        if self.is_initialized == 0 {
            return Ok(0);
        }

        // Upstream uses u128::MAX to avoid precision loss while staying deterministic.
        let total_liquidity = u128::MAX;
        let total_vested_liquidity =
            Self::mul_div_u128_round_down(total_liquidity, self.vesting_percentage.into(), 100)?;

        let mut frequency = self.frequency;
        let mut number_of_periods = self.number_of_periods;
        let mut cliff_duration_from_migration_time = self.cliff_duration_from_migration_time;

        let total_bps_after_cliff = self
            .bps_per_period
            .checked_mul(number_of_periods)
            .context("vesting total_bps_after_cliff overflow")?;
        let total_vesting_liquidity_after_cliff = Self::mul_div_u128_round_down(
            total_vested_liquidity,
            total_bps_after_cliff.into(),
            MAX_BASIS_POINT,
        )?;
        let liquidity_per_period: u128 = if number_of_periods > 0 {
            total_vesting_liquidity_after_cliff
                .checked_div(number_of_periods.into())
                .context("vesting liquidity_per_period division failed")?
        } else {
            0
        };

        if liquidity_per_period == 0 {
            number_of_periods = 0;
            frequency = 0;
            cliff_duration_from_migration_time = cliff_duration_from_migration_time.max(1);
        }

        let cliff_unlock_liquidity = total_vested_liquidity
            .checked_sub(
                liquidity_per_period
                    .checked_mul(number_of_periods.into())
                    .context("vesting cliff_unlock liquidity overflow")?,
            )
            .context("vesting cliff_unlock underflow")?;

        let cliff_point = u64::from(cliff_duration_from_migration_time);
        let unlocked = Self::max_unlocked_liquidity_at_current_point(
            cliff_point,
            liquidity_per_period,
            cliff_unlock_liquidity,
            frequency.into(),
            number_of_periods,
            n_seconds,
        )?;
        let locked = total_vested_liquidity
            .checked_sub(unlocked)
            .context("vesting locked liquidity underflow")?;

        let locked_bps = Self::mul_div_u128_round_down(locked, MAX_BASIS_POINT, total_liquidity)?;
        let locked_bps: u16 = locked_bps
            .try_into()
            .map_err(|_| anyhow::anyhow!("locked bps does not fit u16"))?;
        Ok(locked_bps)
    }
}

impl MeteoraDbcConfigState {
    pub fn total_locked_liquidity_bps_after_n_seconds(
        &self,
        n_seconds: u64,
    ) -> anyhow::Result<u16> {
        let partner_vested = self
            .partner_liquidity_vesting_info
            .liquidity_locked_bps_at_n_seconds(n_seconds)?;
        let creator_vested = self
            .creator_liquidity_vesting_info
            .liquidity_locked_bps_at_n_seconds(n_seconds)?;

        let partner_permanent = u16::from(self.partner_permanent_locked_liquidity_percentage)
            .checked_mul(100)
            .context("partner permanent locked bps overflow")?;
        let creator_permanent = u16::from(self.creator_permanent_locked_liquidity_percentage)
            .checked_mul(100)
            .context("creator permanent locked bps overflow")?;

        partner_vested
            .checked_add(creator_vested)
            .and_then(|v| v.checked_add(partner_permanent))
            .and_then(|v| v.checked_add(creator_permanent))
            .context("total locked liquidity bps overflow")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InitializePoolEvent {
    pub pool: Pubkey,
    pub config: Pubkey,
    pub creator: Pubkey,
    pub base_mint: Pubkey,
    pub pool_type: u8,
    pub activation_point: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Swap2Event {
    pub pool: Pubkey,
    pub config: Pubkey,
    pub trade_direction: u8,
    pub has_referral: bool,
    pub amount_0: u64,
    pub amount_1: u64,
    pub swap_mode: u8,
    pub included_fee_input_amount: u64,
    pub excluded_fee_input_amount: u64,
    pub output_amount: u64,
    pub quote_reserve_amount: u64,
    pub migration_threshold: u64,
    pub current_timestamp: u64,
}

#[derive(Debug)]
pub enum MeteoraDbcEvent {
    InitializePool(Option<InitializePoolEvent>),
    Swap2(Option<Swap2Event>),
    Unknown,
}

fn read_exact<const N: usize>(cur: &mut Cursor<&[u8]>) -> anyhow::Result<[u8; N]> {
    let mut buf = [0u8; N];
    cur.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_pubkey_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Pubkey> {
    Ok(Pubkey::new_from_array(read_exact::<32>(cur)?))
}

fn read_u64_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<u64> {
    Ok(u64::from_le_bytes(read_exact::<8>(cur)?))
}

fn read_u128_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<u128> {
    Ok(u128::from_le_bytes(read_exact::<16>(cur)?))
}

impl InitializePoolEvent {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        Ok(Self {
            pool: read_pubkey_cursor(cur)?,
            config: read_pubkey_cursor(cur)?,
            creator: read_pubkey_cursor(cur)?,
            base_mint: read_pubkey_cursor(cur)?,
            pool_type: read_exact::<1>(cur)?[0],
            activation_point: read_u64_cursor(cur)?,
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(137);
        out.extend_from_slice(self.pool.as_ref());
        out.extend_from_slice(self.config.as_ref());
        out.extend_from_slice(self.creator.as_ref());
        out.extend_from_slice(self.base_mint.as_ref());
        out.push(self.pool_type);
        out.extend_from_slice(&self.activation_point.to_le_bytes());
        out
    }
}

impl Swap2Event {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        let pool = read_pubkey_cursor(cur)?;
        let config = read_pubkey_cursor(cur)?;
        let trade_direction = read_exact::<1>(cur)?[0];
        let has_referral = read_exact::<1>(cur)?[0] != 0;

        let amount_0 = read_u64_cursor(cur)?;
        let amount_1 = read_u64_cursor(cur)?;
        let swap_mode = read_exact::<1>(cur)?[0];

        // SwapResult2
        let included_fee_input_amount = read_u64_cursor(cur)?;
        let excluded_fee_input_amount = read_u64_cursor(cur)?;
        let _amount_left = read_u64_cursor(cur)?;
        let output_amount = read_u64_cursor(cur)?;
        let _next_sqrt_price = read_u128_cursor(cur)?;
        let _trading_fee = read_u64_cursor(cur)?;
        let _protocol_fee = read_u64_cursor(cur)?;
        let _referral_fee = read_u64_cursor(cur)?;

        let quote_reserve_amount = read_u64_cursor(cur)?;
        let migration_threshold = read_u64_cursor(cur)?;
        let current_timestamp = read_u64_cursor(cur)?;

        Ok(Self {
            pool,
            config,
            trade_direction,
            has_referral,
            amount_0,
            amount_1,
            swap_mode,
            included_fee_input_amount,
            excluded_fee_input_amount,
            output_amount,
            quote_reserve_amount,
            migration_threshold,
            current_timestamp,
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.pool.as_ref());
        out.extend_from_slice(self.config.as_ref());
        out.push(self.trade_direction);
        out.push(u8::from(self.has_referral));
        out.extend_from_slice(&self.amount_0.to_le_bytes());
        out.extend_from_slice(&self.amount_1.to_le_bytes());
        out.push(self.swap_mode);

        out.extend_from_slice(&self.included_fee_input_amount.to_le_bytes());
        out.extend_from_slice(&self.excluded_fee_input_amount.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes()); // amount_left
        out.extend_from_slice(&self.output_amount.to_le_bytes());
        out.extend_from_slice(&123456u128.to_le_bytes()); // next_sqrt_price
        out.extend_from_slice(&5u64.to_le_bytes()); // trading_fee
        out.extend_from_slice(&2u64.to_le_bytes()); // protocol_fee
        out.extend_from_slice(&1u64.to_le_bytes()); // referral_fee

        out.extend_from_slice(&self.quote_reserve_amount.to_le_bytes());
        out.extend_from_slice(&self.migration_threshold.to_le_bytes());
        out.extend_from_slice(&self.current_timestamp.to_le_bytes());
        out
    }
}

#[derive(Clone)]
pub struct MeteoraDbc {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
}

impl MeteoraDbc {
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

    fn read_u128(data: &[u8], offset: usize) -> anyhow::Result<u128> {
        let bytes = data
            .get(offset..offset + 16)
            .with_context(|| format!("missing u128 bytes at offset {offset}"))?;
        Ok(u128::from_le_bytes(bytes.try_into()?))
    }

    pub fn decode_virtual_pool_account_data(
        data: &[u8],
    ) -> anyhow::Result<MeteoraDbcVirtualPoolState> {
        anyhow::ensure!(
            data.len() >= DBC_VIRTUAL_POOL_MIN_DECODE_LEN,
            "meteora dbc virtual pool account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == VIRTUAL_POOL_DISCRIM,
            "meteora dbc virtual pool discriminator mismatch"
        );

        Ok(MeteoraDbcVirtualPoolState {
            config: Self::read_pubkey(data, DBC_VIRTUAL_POOL_CONFIG_OFFSET)?,
            creator: Self::read_pubkey(data, DBC_VIRTUAL_POOL_CREATOR_OFFSET)?,
            base_mint: Self::read_pubkey(data, DBC_VIRTUAL_POOL_BASE_MINT_OFFSET)?,
            base_vault: Self::read_pubkey(data, DBC_VIRTUAL_POOL_BASE_VAULT_OFFSET)?,
            quote_vault: Self::read_pubkey(data, DBC_VIRTUAL_POOL_QUOTE_VAULT_OFFSET)?,
            base_reserve: Self::read_u64(data, DBC_VIRTUAL_POOL_BASE_RESERVE_OFFSET)?,
            quote_reserve: Self::read_u64(data, DBC_VIRTUAL_POOL_QUOTE_RESERVE_OFFSET)?,
            sqrt_price: Self::read_u128(data, DBC_VIRTUAL_POOL_SQRT_PRICE_OFFSET)?,
            pool_type: data[DBC_VIRTUAL_POOL_POOL_TYPE_OFFSET],
            is_migrated: data[DBC_VIRTUAL_POOL_IS_MIGRATED_OFFSET],
            migration_progress: data[DBC_VIRTUAL_POOL_MIGRATION_PROGRESS_OFFSET],
        })
    }

    pub fn decode_pool_config_account_data(data: &[u8]) -> anyhow::Result<MeteoraDbcConfigState> {
        anyhow::ensure!(
            data.len() >= DBC_POOL_CONFIG_MIN_DECODE_LEN,
            "meteora dbc config account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == POOL_CONFIG_DISCRIM,
            "meteora dbc config discriminator mismatch"
        );

        Ok(MeteoraDbcConfigState {
            quote_mint: Self::read_pubkey(data, DBC_POOL_CONFIG_QUOTE_MINT_OFFSET)?,
            collect_fee_mode: data[DBC_POOL_CONFIG_COLLECT_FEE_MODE_OFFSET],
            migration_option: data[DBC_POOL_CONFIG_MIGRATION_OPTION_OFFSET],
            token_decimal: data[DBC_POOL_CONFIG_TOKEN_DECIMAL_OFFSET],
            token_type: data[DBC_POOL_CONFIG_TOKEN_TYPE_OFFSET],
            partner_liquidity_vesting_info: DbcLiquidityVestingInfo::decode_from(
                data,
                DBC_POOL_CONFIG_PARTNER_VESTING_INFO_OFFSET,
            )?,
            creator_liquidity_vesting_info: DbcLiquidityVestingInfo::decode_from(
                data,
                DBC_POOL_CONFIG_CREATOR_VESTING_INFO_OFFSET,
            )?,
            partner_permanent_locked_liquidity_percentage: data
                [DBC_POOL_CONFIG_PARTNER_PERMANENT_LOCKED_LIQ_PCT_OFFSET],
            creator_permanent_locked_liquidity_percentage: data
                [DBC_POOL_CONFIG_CREATOR_PERMANENT_LOCKED_LIQ_PCT_OFFSET],
        })
    }

    fn encode_swap_instruction_data(amount_in: u64, min_amount_out: u64) -> Vec<u8> {
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&SWAP_IX_DISCRIM);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        data
    }

    fn ata_for(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(owner, mint, token_program)
    }

    fn derive_pool_authority_pda() -> Pubkey {
        Pubkey::find_program_address(&[POOL_AUTHORITY_SEED], &METEORA_DBC_ID).0
    }

    fn derive_event_authority_pda() -> Pubkey {
        Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], &METEORA_DBC_ID).0
    }

    pub fn price_from_sqrt_price_x64(
        sqrt_price: u128,
        base_decimals: u8,
        quote_decimals: u8,
    ) -> anyhow::Result<f64> {
        anyhow::ensure!(sqrt_price > 0, "invalid meteora dbc sqrt_price");

        let sqrt_ratio = sqrt_price as f64 / (1u128 << 64) as f64;
        let mut price = sqrt_ratio * sqrt_ratio;
        let decimal_adjustment = 10_f64.powi(base_decimals as i32 - quote_decimals as i32);
        price *= decimal_adjustment;

        anyhow::ensure!(
            price.is_finite() && price > 0.0,
            "invalid meteora dbc token price"
        );
        Ok(price)
    }

    fn validate_pool_for_mint(state: &MeteoraDbcPoolState, mint: &Pubkey) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.virtual_pool.base_mint == *mint,
            "meteora dbc pool base mint mismatch (expected {}, got {})",
            mint,
            state.virtual_pool.base_mint
        );
        anyhow::ensure!(
            state.config.quote_mint == WSOL_MINT,
            "meteora dbc pool quote mint is not WSOL"
        );
        anyhow::ensure!(
            state.virtual_pool.is_migrated == 0,
            "meteora dbc pool has migrated"
        );
        Ok(())
    }

    async fn user_token_balance_raw(&self, owner: &Pubkey, mint: &Pubkey) -> anyhow::Result<u64> {
        let token_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .with_context(|| format!("failed to resolve token program for mint {}", mint))?;
        let ata = Self::ata_for(owner, mint, &token_program);
        let amount = self
            .sol
            .rpc_client
            .get_token_account_balance_with_commitment(&ata, CommitmentConfig::confirmed())
            .await?
            .value
            .amount
            .parse::<u64>()?;
        Ok(amount)
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

    pub fn parse_logs<'a>(
        logs: impl Iterator<Item = &'a String>,
        sig: Option<&String>,
    ) -> Vec<MeteoraDbcEvent> {
        let mut events = Vec::new();
        for log in logs {
            if !log.starts_with(SEARCH_FOR) {
                continue;
            }
            let b64 = match decode_b64(log.trim_start_matches(SEARCH_FOR)) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if b64.len() < 8 {
                continue;
            }

            if b64[..8] == EVT_INITIALIZE_POOL_EVENT_DISCRIM {
                let mut cursor = Cursor::new(&b64[8..]);
                match InitializePoolEvent::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(MeteoraDbcEvent::InitializePool(Some(event))),
                    Err(e) => warn!(
                        "Error deserializing meteora dbc initialize-pool event {:?}: {e}",
                        sig.unwrap_or(&"".to_string())
                    ),
                }
            } else if b64[..8] == EVT_SWAP2_EVENT_DISCRIM {
                let mut cursor = Cursor::new(&b64[8..]);
                match Swap2Event::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(MeteoraDbcEvent::Swap2(Some(event))),
                    Err(e) => warn!(
                        "Error deserializing meteora dbc swap2 event {:?}: {e}",
                        sig.unwrap_or(&"".to_string())
                    ),
                }
            } else {
                events.push(MeteoraDbcEvent::Unknown);
            }
        }
        events
    }

    fn parse_event_cpi_instruction_data(
        data: &[u8],
        sig: Option<&String>,
    ) -> Option<MeteoraDbcEvent> {
        if data.len() < 16 || data[..8] != EVENT_CPI_IX_DISCRIM {
            return None;
        }

        let event_discrim: [u8; 8] = data[8..16].try_into().ok()?;
        if event_discrim == EVT_INITIALIZE_POOL_EVENT_DISCRIM {
            let mut cursor = Cursor::new(&data[16..]);
            match InitializePoolEvent::deserialize_from_cursor(&mut cursor) {
                Ok(event) => Some(MeteoraDbcEvent::InitializePool(Some(event))),
                Err(e) => {
                    warn!(
                        "Error deserializing meteora dbc initialize-pool event-cpi {:?}: {e}",
                        sig.unwrap_or(&"".to_string())
                    );
                    None
                }
            }
        } else if event_discrim == EVT_SWAP2_EVENT_DISCRIM {
            let mut cursor = Cursor::new(&data[16..]);
            match Swap2Event::deserialize_from_cursor(&mut cursor) {
                Ok(event) => Some(MeteoraDbcEvent::Swap2(Some(event))),
                Err(e) => {
                    warn!(
                        "Error deserializing meteora dbc swap2 event-cpi {:?}: {e}",
                        sig.unwrap_or(&"".to_string())
                    );
                    None
                }
            }
        } else {
            None
        }
    }

    pub fn parse_inner_instructions(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
        sig: Option<&String>,
    ) -> Vec<MeteoraDbcEvent> {
        let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
            return Vec::new();
        };
        let UiMessage::Parsed(msg) = &ui_tx.message else {
            return Vec::new();
        };
        let Some(meta) = tx.transaction.meta.as_ref() else {
            return Vec::new();
        };
        let OptionSerializer::Some(inner_instructions) = &meta.inner_instructions else {
            return Vec::new();
        };

        let program_id = METEORA_DBC_ID.to_string();
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        let mut out = Vec::new();
        for inner in inner_instructions {
            for ix in &inner.instructions {
                let bytes = match ix {
                    UiInstruction::Parsed(UiParsedInstruction::PartiallyDecoded(decoded)) => {
                        if decoded.program_id != program_id {
                            continue;
                        }
                        bs58::decode(decoded.data.trim()).into_vec().ok()
                    }
                    UiInstruction::Compiled(compiled) => {
                        let program_index = compiled.program_id_index as usize;
                        let Some(program) = account_keys.get(program_index) else {
                            continue;
                        };
                        if *program != program_id {
                            continue;
                        }
                        bs58::decode(compiled.data.trim()).into_vec().ok()
                    }
                    _ => None,
                };

                let Some(bytes) = bytes else {
                    continue;
                };
                if let Some(event) = Self::parse_event_cpi_instruction_data(&bytes, sig) {
                    out.push(event);
                }
            }
        }

        out
    }

    pub fn extract_pool_base_quote_mints_from_transaction(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> Option<(Pubkey, Pubkey, Pubkey)> {
        fn extract_account_pos(
            ix: &UiInstruction,
            program_id: &str,
            account_keys: &[&str],
            pos: usize,
        ) -> Option<Pubkey> {
            match ix {
                UiInstruction::Parsed(UiParsedInstruction::PartiallyDecoded(decoded)) => {
                    if decoded.program_id != program_id {
                        return None;
                    }
                    decoded
                        .accounts
                        .get(pos)
                        .and_then(|v| Pubkey::from_str(v).ok())
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let idx = *compiled.accounts.get(pos)? as usize;
                    let account = account_keys.get(idx)?;
                    Pubkey::from_str(account).ok()
                }
                _ => None,
            }
        }

        let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
            return None;
        };
        let UiMessage::Parsed(msg) = &ui_tx.message else {
            return None;
        };

        let program_id = METEORA_DBC_ID.to_string();
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        for ix in &msg.instructions {
            let Some(pool_authority) = extract_account_pos(ix, &program_id, &account_keys, 0)
            else {
                continue;
            };

            if pool_authority == METEORA_DBC_POOL_AUTHORITY {
                let pool = extract_account_pos(ix, &program_id, &account_keys, 2)?;
                let base_mint = extract_account_pos(ix, &program_id, &account_keys, 7)?;
                let quote_mint = extract_account_pos(ix, &program_id, &account_keys, 8)?;
                return Some((pool, base_mint, quote_mint));
            }

            let Some(pool_authority) = extract_account_pos(ix, &program_id, &account_keys, 1)
            else {
                continue;
            };
            if pool_authority != METEORA_DBC_POOL_AUTHORITY {
                continue;
            }

            let pool = extract_account_pos(ix, &program_id, &account_keys, 5)?;
            let base_mint = extract_account_pos(ix, &program_id, &account_keys, 3)?;
            let quote_mint = extract_account_pos(ix, &program_id, &account_keys, 4)?;
            return Some((pool, base_mint, quote_mint));
        }

        if let Some(meta) = tx.transaction.meta.as_ref()
            && let OptionSerializer::Some(inner_instructions) = &meta.inner_instructions
        {
            for inner in inner_instructions {
                for ix in &inner.instructions {
                    let Some(pool_authority) =
                        extract_account_pos(ix, &program_id, &account_keys, 0)
                    else {
                        continue;
                    };

                    if pool_authority == METEORA_DBC_POOL_AUTHORITY {
                        let pool = extract_account_pos(ix, &program_id, &account_keys, 2)?;
                        let base_mint = extract_account_pos(ix, &program_id, &account_keys, 7)?;
                        let quote_mint = extract_account_pos(ix, &program_id, &account_keys, 8)?;
                        return Some((pool, base_mint, quote_mint));
                    }

                    let Some(pool_authority) =
                        extract_account_pos(ix, &program_id, &account_keys, 1)
                    else {
                        continue;
                    };
                    if pool_authority != METEORA_DBC_POOL_AUTHORITY {
                        continue;
                    }

                    let pool = extract_account_pos(ix, &program_id, &account_keys, 5)?;
                    let base_mint = extract_account_pos(ix, &program_id, &account_keys, 3)?;
                    let quote_mint = extract_account_pos(ix, &program_id, &account_keys, 4)?;
                    return Some((pool, base_mint, quote_mint));
                }
            }
        }

        None
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
                        .get(2)
                        .and_then(|v| Pubkey::from_str(v).ok())
                }
                UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed)) => {
                    if parsed.program_id != program_id {
                        return None;
                    }

                    let info = parsed.parsed.get("info")?;
                    if let Some(value) = info.get("pool").and_then(|v| v.as_str()) {
                        return Pubkey::from_str(value).ok();
                    }
                    None
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let pool_idx = *compiled.accounts.get(2)? as usize;
                    let account = account_keys.get(pool_idx)?;
                    Pubkey::from_str(account).ok()
                }
            }
        }

        let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
            return None;
        };
        let program_id = METEORA_DBC_ID.to_string();

        let UiMessage::Parsed(msg) = &ui_tx.message else {
            return None;
        };
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        for ix in &msg.instructions {
            if let Some(pool) = extract_pool_from_instruction(ix, &program_id, &account_keys) {
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
                if let Some(pool) = extract_pool_from_instruction(ix, &program_id, &account_keys) {
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
                        .get(3)
                        .and_then(|v| Pubkey::from_str(v).ok())
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let src_idx = *compiled.accounts.get(3)? as usize;
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
        let program_id = METEORA_DBC_ID.to_string();

        let UiMessage::Parsed(msg) = &ui_tx.message else {
            return Ok(None);
        };
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        let mut user_source: Option<Pubkey> = None;
        for ix in &msg.instructions {
            user_source = extract_user_source_account(ix, &program_id, &account_keys);
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
                    user_source = extract_user_source_account(ix, &program_id, &account_keys);
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
        if token_account.owner == TOKEN_2022_PROGRAM_ID {
            let state = SplToken2022Account::unpack(&token_account.data)
                .context("failed to parse token-2022 account state")?;
            return Ok(Some(state.mint));
        }

        Ok(None)
    }

    pub async fn fetch_state(&self, pool: &Pubkey) -> anyhow::Result<MeteoraDbcPoolState> {
        let pool_data = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("meteora dbc pool account not found"))?
            .data;

        let virtual_pool = Self::decode_virtual_pool_account_data(&pool_data)?;

        let config_data = self
            .sol
            .rpc_client
            .get_account_with_commitment(&virtual_pool.config, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!(
                "meteora dbc config account not found: {}",
                virtual_pool.config
            ))?
            .data;

        let config = Self::decode_pool_config_account_data(&config_data)?;

        Ok(MeteoraDbcPoolState {
            virtual_pool,
            config,
        })
    }

    pub async fn fetch_wsol_liquidity_raw(
        &self,
        state: &MeteoraDbcPoolState,
    ) -> anyhow::Result<u64> {
        anyhow::ensure!(
            state.config.quote_mint == WSOL_MINT,
            "meteora dbc pool quote mint is not WSOL"
        );
        Ok(state.virtual_pool.quote_reserve)
    }

    pub async fn fetch_price(&self, pool: &Pubkey) -> anyhow::Result<(MeteoraDbcPoolState, f64)> {
        let state = self.fetch_state(pool).await?;

        let base_decimals = self
            .sol
            .get_token_decimals(&state.virtual_pool.base_mint)
            .await
            .unwrap_or(state.config.token_decimal);

        let quote_decimals = if state.config.quote_mint == WSOL_MINT {
            9
        } else {
            self.sol
                .get_token_decimals(&state.config.quote_mint)
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch quote mint decimals for {}",
                        state.config.quote_mint
                    )
                })?
        };

        let price = match Self::price_from_sqrt_price_x64(
            state.virtual_pool.sqrt_price,
            base_decimals,
            quote_decimals,
        ) {
            Ok(price) => price,
            Err(_) => {
                anyhow::ensure!(
                    state.virtual_pool.base_reserve > 0 && state.virtual_pool.quote_reserve > 0,
                    "meteora dbc pool has no liquidity"
                );

                let base_reserve =
                    state.virtual_pool.base_reserve as f64 / 10_f64.powi(base_decimals as i32);
                let quote_reserve =
                    state.virtual_pool.quote_reserve as f64 / 10_f64.powi(quote_decimals as i32);
                anyhow::ensure!(base_reserve > 0.0, "invalid meteora dbc base reserve");
                quote_reserve / base_reserve
            }
        };

        anyhow::ensure!(
            price.is_finite() && price > 0.0,
            "invalid meteora dbc token price"
        );

        Ok((state, price))
    }

    pub async fn get_mint_from_pool(&self, pool: &Pubkey) -> anyhow::Result<Pubkey> {
        let state = self.fetch_state(pool).await?;
        Ok(state.virtual_pool.base_mint)
    }

    pub async fn find_pools_by_mint(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(DBC_VIRTUAL_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, VIRTUAL_POOL_DISCRIM.as_ref())),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    DBC_VIRTUAL_POOL_BASE_MINT_OFFSET,
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

        let pools = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&METEORA_DBC_ID, cfg)
            .await?;

        let mut out = BTreeSet::new();
        for (pool, _) in pools {
            if let Some(quote) = quote_mint {
                let state = match self.fetch_state(&pool).await {
                    Ok(state) => state,
                    Err(_) => continue,
                };
                if state.config.quote_mint != *quote {
                    continue;
                }
            }
            out.insert(pool);
        }

        Ok(out.into_iter().collect())
    }

    pub async fn find_pools_by_creator(&self, creator: &Pubkey) -> anyhow::Result<Vec<Pubkey>> {
        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(DBC_VIRTUAL_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    DBC_VIRTUAL_POOL_CREATOR_OFFSET,
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

        let accounts = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&METEORA_DBC_ID, cfg)
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
            if state.virtual_pool.is_migrated != 0 {
                continue;
            }
            let liq = if state.config.quote_mint == WSOL_MINT {
                state.virtual_pool.quote_reserve
            } else {
                continue;
            };
            if liq >= min_liquidity_raw && liq >= best_liquidity {
                best_liquidity = liq;
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
        anyhow::ensure!(price > 0.0, "meteora dbc buy price must be > 0");
        anyhow::ensure!(sol_amount_in > 0.0, "meteora dbc buy amount must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch meteora dbc state for {}", pool))?;
        Self::validate_pool_for_mint(&state, mint)?;

        let output_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .context("failed to resolve output token program for meteora dbc buy")?;
        let expected_base_program = if state.virtual_pool.pool_type == 0 {
            TOKEN_PROGRAM_ID
        } else {
            TOKEN_2022_PROGRAM_ID
        };
        anyhow::ensure!(
            output_program == expected_base_program,
            "meteora dbc base token program mismatch"
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
        anyhow::ensure!(amount_in > 0, "meteora dbc buy amount is too small");

        let mint_decimals = self
            .sol
            .get_token_decimals(mint)
            .await
            .unwrap_or(state.config.token_decimal);
        let expected_tokens_out_ui = sol_amount_in / price;
        let min_amount_out = ((expected_tokens_out_ui * (1.0 - slippage_pct)).max(0.0)
            * 10_f64.powi(mint_decimals as i32))
        .floor() as u64;
        let min_amount_out = min_amount_out.max(1);

        ixs.push(system_instruction_if::transfer(
            &buyer, &input_ata, amount_in,
        ));
        ixs.push(sync_native(&input_program, &input_ata)?);

        let pool_authority = Self::derive_pool_authority_pda();
        let event_authority = Self::derive_event_authority_pda();
        let mut accounts = vec![
            AccountMeta::new_readonly(pool_authority, false),
            AccountMeta::new_readonly(state.virtual_pool.config, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(state.virtual_pool.base_vault, false),
            AccountMeta::new(state.virtual_pool.quote_vault, false),
            AccountMeta::new_readonly(state.virtual_pool.base_mint, false),
            AccountMeta::new_readonly(state.config.quote_mint, false),
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(output_program, false),
            AccountMeta::new_readonly(input_program, false),
            AccountMeta::new_readonly(METEORA_DBC_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DBC_ID, false),
        ];
        // Keep this sysvar available for pools using rate-limiter checks.
        accounts.push(AccountMeta::new_readonly(sysvar_instructions::id(), false));

        let recent_fees = self
            .sol
            .resolve_priority_fee(
                priority_fee_override,
                &accounts
                    .iter()
                    .map(|acc| acc.pubkey)
                    .collect::<Vec<Pubkey>>(),
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            )
            .await
            .context("failed to resolve priority fee for meteora dbc buy")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_instruction_data(amount_in, min_amount_out);
        ixs.push(Instruction {
            program_id: METEORA_DBC_ID,
            accounts,
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
        anyhow::ensure!(price > 0.0, "meteora dbc sell price must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch meteora dbc state for {}", pool))?;
        Self::validate_pool_for_mint(&state, mint)?;

        let input_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .context("failed to resolve base token program for meteora dbc sell")?;
        let expected_base_program = if state.virtual_pool.pool_type == 0 {
            TOKEN_PROGRAM_ID
        } else {
            TOKEN_2022_PROGRAM_ID
        };
        anyhow::ensure!(
            input_program == expected_base_program,
            "meteora dbc base token program mismatch"
        );

        let output_program = TOKEN_PROGRAM_ID;
        let sell_pct = sell_pct.clamp(1, 100);

        let input_ata = Self::ata_for(&buyer, mint, &input_program);
        let output_ata = Self::ata_for(&buyer, &WSOL_MINT, &output_program);

        let token_balance_raw = self
            .user_token_balance_raw(&buyer, mint)
            .await
            .context("failed to fetch token balance for meteora dbc sell")?;
        anyhow::ensure!(
            token_balance_raw > 0,
            "no token balance for meteora dbc sell"
        );

        let amount_in = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            amount_in > 0,
            "meteora dbc sell amount is too small for requested percentage"
        );

        let mint_decimals = self
            .sol
            .get_token_decimals(mint)
            .await
            .unwrap_or(state.config.token_decimal);
        let slippage_pct = Self::normalize_slippage(slippage);
        let amount_in_ui = amount_in as f64 / 10_f64.powi(mint_decimals as i32);
        let min_sol_output = (amount_in_ui * price * (1.0 - slippage_pct) * 1e9).floor() as u64;
        let min_sol_output = min_sol_output.max(1);

        let mut ixs = vec![create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &output_program,
        )];

        let pool_authority = Self::derive_pool_authority_pda();
        let event_authority = Self::derive_event_authority_pda();
        let mut accounts = vec![
            AccountMeta::new_readonly(pool_authority, false),
            AccountMeta::new_readonly(state.virtual_pool.config, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(state.virtual_pool.base_vault, false),
            AccountMeta::new(state.virtual_pool.quote_vault, false),
            AccountMeta::new_readonly(state.virtual_pool.base_mint, false),
            AccountMeta::new_readonly(state.config.quote_mint, false),
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(input_program, false),
            AccountMeta::new_readonly(output_program, false),
            AccountMeta::new_readonly(METEORA_DBC_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DBC_ID, false),
        ];
        accounts.push(AccountMeta::new_readonly(sysvar_instructions::id(), false));

        let recent_fees = self
            .sol
            .resolve_priority_fee(
                priority_fee_override,
                &accounts
                    .iter()
                    .map(|acc| acc.pubkey)
                    .collect::<Vec<Pubkey>>(),
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            )
            .await
            .context("failed to resolve priority fee for meteora dbc sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_instruction_data(amount_in, min_sol_output);
        ixs.push(Instruction {
            program_id: METEORA_DBC_ID,
            accounts,
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

    fn synthetic_virtual_pool_account_bytes() -> Vec<u8> {
        let mut data = vec![0u8; DBC_VIRTUAL_POOL_ACCOUNT_LEN];
        data[..8].copy_from_slice(&VIRTUAL_POOL_DISCRIM);

        let config = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        let base_mint = Pubkey::new_unique();
        let base_vault = Pubkey::new_unique();
        let quote_vault = Pubkey::new_unique();

        data[DBC_VIRTUAL_POOL_CONFIG_OFFSET..DBC_VIRTUAL_POOL_CONFIG_OFFSET + 32]
            .copy_from_slice(config.as_ref());
        data[DBC_VIRTUAL_POOL_CREATOR_OFFSET..DBC_VIRTUAL_POOL_CREATOR_OFFSET + 32]
            .copy_from_slice(creator.as_ref());
        data[DBC_VIRTUAL_POOL_BASE_MINT_OFFSET..DBC_VIRTUAL_POOL_BASE_MINT_OFFSET + 32]
            .copy_from_slice(base_mint.as_ref());
        data[DBC_VIRTUAL_POOL_BASE_VAULT_OFFSET..DBC_VIRTUAL_POOL_BASE_VAULT_OFFSET + 32]
            .copy_from_slice(base_vault.as_ref());
        data[DBC_VIRTUAL_POOL_QUOTE_VAULT_OFFSET..DBC_VIRTUAL_POOL_QUOTE_VAULT_OFFSET + 32]
            .copy_from_slice(quote_vault.as_ref());

        data[DBC_VIRTUAL_POOL_BASE_RESERVE_OFFSET..DBC_VIRTUAL_POOL_BASE_RESERVE_OFFSET + 8]
            .copy_from_slice(&123_456_789u64.to_le_bytes());
        data[DBC_VIRTUAL_POOL_QUOTE_RESERVE_OFFSET..DBC_VIRTUAL_POOL_QUOTE_RESERVE_OFFSET + 8]
            .copy_from_slice(&987_654_321u64.to_le_bytes());
        data[DBC_VIRTUAL_POOL_SQRT_PRICE_OFFSET..DBC_VIRTUAL_POOL_SQRT_PRICE_OFFSET + 16]
            .copy_from_slice(&(1u128 << 64).to_le_bytes());

        data[DBC_VIRTUAL_POOL_POOL_TYPE_OFFSET] = 0;
        data[DBC_VIRTUAL_POOL_IS_MIGRATED_OFFSET] = 0;
        data[DBC_VIRTUAL_POOL_MIGRATION_PROGRESS_OFFSET] = 1;

        data
    }

    fn synthetic_pool_config_account_bytes() -> Vec<u8> {
        let mut data = vec![0u8; DBC_POOL_CONFIG_ACCOUNT_LEN];
        data[..8].copy_from_slice(&POOL_CONFIG_DISCRIM);
        data[DBC_POOL_CONFIG_QUOTE_MINT_OFFSET..DBC_POOL_CONFIG_QUOTE_MINT_OFFSET + 32]
            .copy_from_slice(WSOL_MINT.as_ref());
        data[DBC_POOL_CONFIG_COLLECT_FEE_MODE_OFFSET] = 1;
        data[DBC_POOL_CONFIG_MIGRATION_OPTION_OFFSET] = 1;
        data[DBC_POOL_CONFIG_TOKEN_DECIMAL_OFFSET] = 6;
        data[DBC_POOL_CONFIG_TOKEN_TYPE_OFFSET] = 0;
        data
    }

    #[test]
    fn test_meteora_dbc_discriminators_match_anchor_layout() {
        assert_eq!(SWAP_IX_DISCRIM, anchor_discriminator("global", "swap"));
        assert_eq!(
            VIRTUAL_POOL_DISCRIM,
            anchor_discriminator("account", "VirtualPool")
        );
        assert_eq!(
            POOL_CONFIG_DISCRIM,
            anchor_discriminator("account", "PoolConfig")
        );
        assert_eq!(
            EVT_INITIALIZE_POOL_EVENT_DISCRIM,
            anchor_discriminator("event", "EvtInitializePool")
        );
        assert_eq!(
            EVT_SWAP2_EVENT_DISCRIM,
            anchor_discriminator("event", "EvtSwap2")
        );
    }

    #[test]
    fn test_meteora_dbc_program_constants() {
        assert_eq!(
            METEORA_DBC_ID,
            Pubkey::from_str("dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN").unwrap()
        );
        assert_eq!(
            METEORA_DBC_POOL_AUTHORITY,
            Pubkey::from_str("FhVo3mqL8PW5pH5U2CN4XE33DokiyZnUwuGpH2hmHLuM").unwrap()
        );
        assert_eq!(
            MeteoraDbc::derive_pool_authority_pda(),
            METEORA_DBC_POOL_AUTHORITY
        );
    }

    #[test]
    fn test_meteora_dbc_encode_swap_instruction_data_layout() {
        let data = MeteoraDbc::encode_swap_instruction_data(1_234, 9_876);
        assert_eq!(data.len(), 24);
        assert_eq!(&data[..8], &SWAP_IX_DISCRIM);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 1_234);
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 9_876);
    }

    #[test]
    fn test_meteora_dbc_normalize_slippage() {
        assert_eq!(MeteoraDbc::normalize_slippage(15.0), 0.15);
        assert_eq!(MeteoraDbc::normalize_slippage(0.2), 0.2);
        assert_eq!(MeteoraDbc::normalize_slippage(0.0), 0.01);
        assert_eq!(MeteoraDbc::normalize_slippage(120.0), 0.99);
    }

    #[test]
    fn test_meteora_dbc_decode_virtual_pool_state() {
        let data = synthetic_virtual_pool_account_bytes();
        let state = MeteoraDbc::decode_virtual_pool_account_data(&data).expect("decode must pass");

        assert_eq!(state.base_reserve, 123_456_789);
        assert_eq!(state.quote_reserve, 987_654_321);
        assert_eq!(state.sqrt_price, 1u128 << 64);
        assert_eq!(state.pool_type, 0);
        assert_eq!(state.is_migrated, 0);
        assert_eq!(state.migration_progress, 1);
        assert_ne!(state.base_mint, Pubkey::default());
    }

    #[test]
    fn test_meteora_dbc_decode_pool_config_state() {
        let data = synthetic_pool_config_account_bytes();
        let state = MeteoraDbc::decode_pool_config_account_data(&data).expect("decode must pass");

        assert_eq!(state.quote_mint, WSOL_MINT);
        assert_eq!(state.collect_fee_mode, 1);
        assert_eq!(state.migration_option, 1);
        assert_eq!(state.token_decimal, 6);
        assert_eq!(state.token_type, 0);
    }

    #[test]
    fn test_meteora_dbc_price_from_sqrt_price_x64() {
        let price = MeteoraDbc::price_from_sqrt_price_x64(1u128 << 64, 6, 9).unwrap();
        assert!((price - 0.001).abs() < 1e-12);
    }

    #[test]
    fn test_meteora_dbc_parse_initialize_pool_event_fixture() {
        let event = InitializePoolEvent {
            pool: Pubkey::new_unique(),
            config: Pubkey::new_unique(),
            creator: Pubkey::new_unique(),
            base_mint: Pubkey::new_unique(),
            pool_type: 0,
            activation_point: 1_700_000_000,
        };
        let encoded = encode_fixture_event(&EVT_INITIALIZE_POOL_EVENT_DISCRIM, &event.to_bytes());
        let logs = vec![format!("{}{}", SEARCH_FOR, encoded)];

        let events = MeteoraDbc::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDbcEvent::InitializePool(Some(decoded)) => {
                assert_eq!(decoded.pool, event.pool);
                assert_eq!(decoded.config, event.config);
                assert_eq!(decoded.creator, event.creator);
                assert_eq!(decoded.base_mint, event.base_mint);
                assert_eq!(decoded.pool_type, event.pool_type);
                assert_eq!(decoded.activation_point, event.activation_point);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_meteora_dbc_parse_swap2_event_fixture() {
        let event = Swap2Event {
            pool: Pubkey::new_unique(),
            config: Pubkey::new_unique(),
            trade_direction: 1,
            has_referral: false,
            amount_0: 50,
            amount_1: 40,
            swap_mode: 0,
            included_fee_input_amount: 777,
            excluded_fee_input_amount: 666,
            output_amount: 39,
            quote_reserve_amount: 12345,
            migration_threshold: 999_999,
            current_timestamp: 1_700_000_000,
        };
        let encoded = encode_fixture_event(&EVT_SWAP2_EVENT_DISCRIM, &event.to_bytes());
        let logs = vec![format!("{}{}", SEARCH_FOR, encoded)];

        let events = MeteoraDbc::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDbcEvent::Swap2(Some(decoded)) => {
                assert_eq!(decoded.pool, event.pool);
                assert_eq!(decoded.config, event.config);
                assert_eq!(decoded.trade_direction, event.trade_direction);
                assert_eq!(decoded.has_referral, event.has_referral);
                assert_eq!(decoded.amount_0, event.amount_0);
                assert_eq!(decoded.amount_1, event.amount_1);
                assert_eq!(decoded.swap_mode, event.swap_mode);
                assert_eq!(
                    decoded.included_fee_input_amount,
                    event.included_fee_input_amount
                );
                assert_eq!(
                    decoded.excluded_fee_input_amount,
                    event.excluded_fee_input_amount
                );
                assert_eq!(decoded.output_amount, event.output_amount);
                assert_eq!(decoded.quote_reserve_amount, event.quote_reserve_amount);
                assert_eq!(decoded.migration_threshold, event.migration_threshold);
                assert_eq!(decoded.current_timestamp, event.current_timestamp);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_meteora_dbc_parse_logs_unknown_event() {
        let logs = vec![format!("{}{}", SEARCH_FOR, B64.encode(vec![1u8; 64]))];
        let events = MeteoraDbc::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MeteoraDbcEvent::Unknown));
    }

    #[test]
    fn test_meteora_dbc_extract_pool_from_inner_instruction_fixture() {
        let pool = Pubkey::new_unique();
        let config = Pubkey::new_unique();

        let tx_json = json!({
            "slot": 1,
            "transaction": {
                "signatures": ["1111111111111111111111111111111111111111111111111111111111111111"],
                "message": {
                    "accountKeys": [
                        {
                            "pubkey": METEORA_DBC_POOL_AUTHORITY.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": config.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": pool.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": "So11111111111111111111111111111111111111112",
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": METEORA_DBC_ID.to_string(),
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
                "preBalances": [0, 0, 0, 0, 0],
                "postBalances": [0, 0, 0, 0, 0],
                "innerInstructions": [
                    {
                        "index": 0,
                        "instructions": [
                            {
                                "programId": METEORA_DBC_ID.to_string(),
                                "accounts": [
                                    METEORA_DBC_POOL_AUTHORITY.to_string(),
                                    config.to_string(),
                                    pool.to_string(),
                                    "So11111111111111111111111111111111111111112"
                                ],
                                "data": ""
                            }
                        ]
                    }
                ],
                "logMessages": []
            },
            "version": "legacy",
            "blockTime": null
        });

        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(tx_json).expect("fixture tx must deserialize");

        assert_eq!(MeteoraDbc::extract_pool_from_transaction(&tx), Some(pool));
    }

    #[test]
    fn test_meteora_dbc_parse_inner_instructions_swap2_event_cpi_fixture() {
        let event = Swap2Event {
            pool: Pubkey::new_unique(),
            config: Pubkey::new_unique(),
            trade_direction: 1,
            has_referral: true,
            amount_0: 50,
            amount_1: 40,
            swap_mode: 0,
            included_fee_input_amount: 777,
            excluded_fee_input_amount: 666,
            output_amount: 39,
            quote_reserve_amount: 12345,
            migration_threshold: 999_999,
            current_timestamp: 1_700_000_000,
        };

        let mut payload = Vec::new();
        payload.extend_from_slice(&EVENT_CPI_IX_DISCRIM);
        payload.extend_from_slice(&EVT_SWAP2_EVENT_DISCRIM);
        payload.extend_from_slice(&event.to_bytes());
        let data = bs58::encode(payload).into_string();

        let tx_json = json!({
            "slot": 1,
            "transaction": {
                "signatures": ["1111111111111111111111111111111111111111111111111111111111111111"],
                "message": {
                    "accountKeys": [
                        {
                            "pubkey": METEORA_DBC_ID.to_string(),
                            "writable": false,
                            "signer": false
                        }
                    ],
                    "recentBlockhash": "11111111111111111111111111111111",
                    "instructions": []
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
                                "programId": METEORA_DBC_ID.to_string(),
                                "accounts": [
                                    Pubkey::new_unique().to_string()
                                ],
                                "data": data
                            }
                        ]
                    }
                ],
                "logMessages": []
            },
            "version": "legacy",
            "blockTime": null
        });

        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(tx_json).expect("fixture tx must deserialize");

        let events = MeteoraDbc::parse_inner_instructions(&tx, None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDbcEvent::Swap2(Some(decoded)) => {
                assert_eq!(decoded.pool, event.pool);
                assert_eq!(decoded.config, event.config);
                assert_eq!(decoded.trade_direction, event.trade_direction);
                assert_eq!(decoded.has_referral, event.has_referral);
                assert_eq!(decoded.amount_0, event.amount_0);
                assert_eq!(decoded.amount_1, event.amount_1);
                assert_eq!(decoded.swap_mode, event.swap_mode);
                assert_eq!(
                    decoded.included_fee_input_amount,
                    event.included_fee_input_amount
                );
                assert_eq!(
                    decoded.excluded_fee_input_amount,
                    event.excluded_fee_input_amount
                );
                assert_eq!(decoded.output_amount, event.output_amount);
                assert_eq!(decoded.quote_reserve_amount, event.quote_reserve_amount);
                assert_eq!(decoded.migration_threshold, event.migration_threshold);
                assert_eq!(decoded.current_timestamp, event.current_timestamp);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_meteora_dbc_extract_pool_base_quote_mints_from_transaction_swap_fixture() {
        let pool = Pubkey::new_unique();
        let config = Pubkey::new_unique();
        let base_mint = Pubkey::new_unique();
        let quote_mint = WSOL_MINT;

        let tx_json = json!({
            "slot": 1,
            "transaction": {
                "signatures": ["1111111111111111111111111111111111111111111111111111111111111111"],
                "message": {
                    "accountKeys": [
                        {
                            "pubkey": METEORA_DBC_ID.to_string(),
                            "writable": false,
                            "signer": false
                        }
                    ],
                    "recentBlockhash": "11111111111111111111111111111111",
                    "instructions": [
                        {
                            "programId": METEORA_DBC_ID.to_string(),
                            "accounts": [
                                METEORA_DBC_POOL_AUTHORITY.to_string(),
                                config.to_string(),
                                pool.to_string(),
                                Pubkey::new_unique().to_string(),
                                Pubkey::new_unique().to_string(),
                                Pubkey::new_unique().to_string(),
                                Pubkey::new_unique().to_string(),
                                base_mint.to_string(),
                                quote_mint.to_string()
                            ],
                            "data": ""
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
                "innerInstructions": [],
                "logMessages": []
            },
            "version": "legacy",
            "blockTime": null
        });

        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(tx_json).expect("fixture tx must deserialize");

        assert_eq!(
            MeteoraDbc::extract_pool_base_quote_mints_from_transaction(&tx),
            Some((pool, base_mint, quote_mint))
        );
    }

    #[test]
    fn test_meteora_dbc_extract_pool_base_quote_mints_from_transaction_inner_swap_fixture() {
        let pool = Pubkey::new_unique();
        let config = Pubkey::new_unique();
        let base_mint = Pubkey::new_unique();
        let quote_mint = WSOL_MINT;

        let tx_json = json!({
            "slot": 1,
            "transaction": {
                "signatures": ["1111111111111111111111111111111111111111111111111111111111111111"],
                "message": {
                    "accountKeys": [
                        {
                            "pubkey": METEORA_DBC_ID.to_string(),
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
                                "programId": METEORA_DBC_ID.to_string(),
                                "accounts": [
                                    METEORA_DBC_POOL_AUTHORITY.to_string(),
                                    config.to_string(),
                                    pool.to_string(),
                                    Pubkey::new_unique().to_string(),
                                    Pubkey::new_unique().to_string(),
                                    Pubkey::new_unique().to_string(),
                                    Pubkey::new_unique().to_string(),
                                    base_mint.to_string(),
                                    quote_mint.to_string()
                                ],
                                "data": ""
                            }
                        ]
                    }
                ],
                "logMessages": []
            },
            "version": "legacy",
            "blockTime": null
        });

        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(tx_json).expect("fixture tx must deserialize");

        assert_eq!(
            MeteoraDbc::extract_pool_base_quote_mints_from_transaction(&tx),
            Some((pool, base_mint, quote_mint))
        );
    }

    #[test]
    fn test_meteora_dbc_migrated_pool_state_is_marked_migrated() {
        let mut virtual_pool =
            MeteoraDbc::decode_virtual_pool_account_data(&synthetic_virtual_pool_account_bytes())
                .expect("decode must pass");
        virtual_pool.is_migrated = 1;

        let config =
            MeteoraDbc::decode_pool_config_account_data(&synthetic_pool_config_account_bytes())
                .expect("decode must pass");

        let state = MeteoraDbcPoolState {
            virtual_pool,
            config,
        };

        assert_eq!(state.config.quote_mint, WSOL_MINT);
        assert_eq!(state.virtual_pool.quote_reserve, 987_654_321);
        assert_ne!(state.virtual_pool.is_migrated, 0);
    }
}
