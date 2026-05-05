use crate::core::sol::{
    DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SolHook, TOKEN_2022_PROGRAM_ID,
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
    pubkey::Pubkey,
};
use solana_signer::Signer;
use solana_system_interface::instruction as system_instruction_if;
use spl_associated_token_account::instruction::{
    create_associated_token_account, create_associated_token_account_idempotent,
};
use spl_token_2022::instruction::sync_native;
use std::collections::BTreeSet;
use std::io::Cursor;
use std::io::Read;
use std::sync::Arc;

pub const RAYDIUM_CLMM_ID: Pubkey = crate::core::cluster::RAYDIUM_CLMM_PROGRAM_ID_MAINNET;
pub const RAYDIUM_CLMM_DEVNET_ID: Pubkey = crate::core::cluster::RAYDIUM_CLMM_PROGRAM_ID_DEVNET;
pub const MEMO_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

pub const SWAP_V2_IX_DISCRIM: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];
pub const POOL_STATE_DISCRIM: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];
pub const AMM_CONFIG_DISCRIM: [u8; 8] = [218, 244, 33, 104, 203, 203, 43, 111];
pub const POOL_CREATED_EVENT_DISCRIM: [u8; 8] = [25, 94, 75, 47, 112, 99, 53, 63];
pub const SWAP_EVENT_DISCRIM: [u8; 8] = [64, 198, 205, 232, 38, 8, 113, 226];
pub const RAYDIUM_CLMM_CREATE_POOL_IX_DISCRIM: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];
// Raydium SDK v2 `ClmmInstrument.openPositionFromBaseInstruction` discriminator.
pub const RAYDIUM_CLMM_OPEN_POSITION_IX_DISCRIM: [u8; 8] = [77, 184, 74, 214, 112, 86, 241, 199];

pub const SEARCH_FOR: &str = "Program data: ";
pub const POOL_SEED: &[u8] = b"pool";
pub const POOL_VAULT_SEED: &[u8] = b"pool_vault";
pub const POSITION_SEED: &[u8] = b"position";
pub const TICK_ARRAY_SEED: &[u8] = b"tick_array";
pub const POOL_TICK_ARRAY_BITMAP_SEED: &[u8] = b"pool_tick_array_bitmap_extension";
pub const OBSERVATION_SEED: &[u8] = b"observation";

pub const CLMM_POOL_ACCOUNT_LEN: usize = 1544;
pub const CLMM_POOL_OWNER_OFFSET: usize = 41;
pub const CLMM_POOL_MINT_A_OFFSET: usize = 73;
pub const CLMM_POOL_MINT_B_OFFSET: usize = 105;
pub const TICK_ARRAY_SIZE: i32 = 60;
const DEFAULT_TICK_ARRAY_CANDIDATES: usize = 5;
// Raydium SDK v2 clmm tick bounds (`utils/constants.ts`).
pub const CLMM_MIN_TICK: i32 = -443_636;
pub const CLMM_MAX_TICK: i32 = 443_636;

pub fn decode_amm_config_tick_spacing(data: &[u8]) -> anyhow::Result<u16> {
    const TICK_SPACING_OFFSET: usize = AMM_CONFIG_TICK_SPACING_OFFSET;
    anyhow::ensure!(
        data.len() >= TICK_SPACING_OFFSET + 2,
        "amm config too short"
    );
    anyhow::ensure!(
        data[..8] == AMM_CONFIG_DISCRIM,
        "amm config discriminator mismatch"
    );
    let bytes: [u8; 2] = data[TICK_SPACING_OFFSET..TICK_SPACING_OFFSET + 2]
        .try_into()
        .context("failed to read tick spacing bytes")?;
    Ok(u16::from_le_bytes(bytes))
}

// Layout matches upstream Raydium CLMM `AmmConfig`:
// - 8 bytes discriminator
// - 1 byte bump
// - 2 bytes index
// - 32 bytes owner
// - 4 bytes protocol_fee_rate
// - 4 bytes trade_fee_rate
// - 2 bytes tick_spacing (u16 LE)
pub const AMM_CONFIG_TICK_SPACING_OFFSET: usize = 8 + 1 + 2 + 32 + 4 + 4;

pub const AMM_CONFIGS: [Pubkey; 18] = [
    Pubkey::from_str_const("9iFER3bpjf1PTTCQCfTRu17EJgvsxo9pVyA9QWwEuX4x"),
    Pubkey::from_str_const("EdPxg8QaeFSrTYqdWJn6Kezwy9McWncTYueD9eMGCuzR"),
    Pubkey::from_str_const("9EeWRCL8CJnikDFCDzG8rtmBs5KQR1jEYKCR5rRZ2NEi"),
    Pubkey::from_str_const("3h2e43PunVA5K34vwKCLHWhZF4aZpyaC9RmxvshGAQpL"),
    Pubkey::from_str_const("3XCQJQryqpDvvZBfGxR7CLAw5dpGJ9aa7kt1jRLdyxuZ"),
    Pubkey::from_str_const("DrdecJVzkaRsf1TQu1g7iFncaokikVTHqpzPjenjRySY"),
    Pubkey::from_str_const("J8u7HvA1g1p2CdhBFdsnTxDzGkekRpdw4GrL9MKU2D3U"),
    Pubkey::from_str_const("RPxHtdN5V7ajwkoG6NnwSBAeaX5k9giY37dpp98xTjD"),
    Pubkey::from_str_const("9WjDVMHWCirG9jkchbetHTnSzdXbAPnD9bsoGRcz1xUw"),
    Pubkey::from_str_const("FMrUDGjEe1izXPbn8SZPNjMfB5JvvhVq5ymmpZDebB5R"),
    Pubkey::from_str_const("E64NGkDLLCdQ2yFNPcavaKptrEgmiQaNykUuLC1Qgwyp"),
    Pubkey::from_str_const("Y6YhgJbt9FRk3JVjwdZtsioVCJwCKhy1hum8HMDYyB1"),
    Pubkey::from_str_const("47Nq74YtwjVeTQF6KFKRKU4cY1Vd5AXBHpYRkubkDLZi"),
    Pubkey::from_str_const("DQeN7dZyQvXKT7YwmgqyuC7AYFkwMoP7RwtucsDEdfYZ"),
    Pubkey::from_str_const("A1BBtTYJd4i3xU8D6Tc2FzU6ZN4oXZWXKZnCxwbHXr8x"),
    Pubkey::from_str_const("Gex2NJRS3jVLPfbzSFM5d5DRsNoL5ynnwT1TXoDEhanz"),
    Pubkey::from_str_const("CDpiwv9eLsRvvuzZEJ8CBtK14wdvkSnkub4vmGtzzdK8"),
    Pubkey::from_str_const("6tBc3ABLaYTTWu94DiRD5PWi92HML34UpAQ8pPTYgudw"),
];

#[derive(Debug, Clone)]
pub struct ClmmPoolState {
    pub bump: u8,
    pub amm_config: Pubkey,
    pub owner: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub vault_a: Pubkey,
    pub vault_b: Pubkey,
    pub observation_id: Pubkey,
    pub mint_decimals_a: u8,
    pub mint_decimals_b: u8,
    pub tick_spacing: u16,
    pub liquidity: u128,
    pub sqrt_price_x64: u128,
    pub tick_current: i32,
    pub status: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PoolCreatedEvent {
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,
    pub tick_spacing: u16,
    pub pool_state: Pubkey,
    pub sqrt_price_x64: u128,
    pub tick: i32,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwapEvent {
    pub pool_state: Pubkey,
    pub sender: Pubkey,
    pub token_account_0: Pubkey,
    pub token_account_1: Pubkey,
    pub amount_0: u64,
    pub transfer_fee_0: u64,
    pub amount_1: u64,
    pub transfer_fee_1: u64,
    pub zero_for_one: bool,
    pub sqrt_price_x64: u128,
    pub liquidity: u128,
    pub tick: i32,
}

#[derive(Debug)]
pub enum RaydiumClmmEvent {
    PoolCreated(Option<PoolCreatedEvent>),
    Swap(Option<SwapEvent>),
    Unknown,
}

impl PoolCreatedEvent {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        Ok(Self {
            token_mint_0: read_pubkey_cursor(cur)?,
            token_mint_1: read_pubkey_cursor(cur)?,
            tick_spacing: read_u16_cursor(cur)?,
            pool_state: read_pubkey_cursor(cur)?,
            sqrt_price_x64: read_u128_cursor(cur)?,
            tick: read_i32_cursor(cur)?,
            token_vault_0: read_pubkey_cursor(cur)?,
            token_vault_1: read_pubkey_cursor(cur)?,
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(154);
        out.extend_from_slice(self.token_mint_0.as_ref());
        out.extend_from_slice(self.token_mint_1.as_ref());
        out.extend_from_slice(&self.tick_spacing.to_le_bytes());
        out.extend_from_slice(self.pool_state.as_ref());
        out.extend_from_slice(&self.sqrt_price_x64.to_le_bytes());
        out.extend_from_slice(&self.tick.to_le_bytes());
        out.extend_from_slice(self.token_vault_0.as_ref());
        out.extend_from_slice(self.token_vault_1.as_ref());
        out
    }
}

impl SwapEvent {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        Ok(Self {
            pool_state: read_pubkey_cursor(cur)?,
            sender: read_pubkey_cursor(cur)?,
            token_account_0: read_pubkey_cursor(cur)?,
            token_account_1: read_pubkey_cursor(cur)?,
            amount_0: read_u64_cursor(cur)?,
            transfer_fee_0: read_u64_cursor(cur)?,
            amount_1: read_u64_cursor(cur)?,
            transfer_fee_1: read_u64_cursor(cur)?,
            zero_for_one: read_bool_cursor(cur)?,
            sqrt_price_x64: read_u128_cursor(cur)?,
            liquidity: read_u128_cursor(cur)?,
            tick: read_i32_cursor(cur)?,
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(205);
        out.extend_from_slice(self.pool_state.as_ref());
        out.extend_from_slice(self.sender.as_ref());
        out.extend_from_slice(self.token_account_0.as_ref());
        out.extend_from_slice(self.token_account_1.as_ref());
        out.extend_from_slice(&self.amount_0.to_le_bytes());
        out.extend_from_slice(&self.transfer_fee_0.to_le_bytes());
        out.extend_from_slice(&self.amount_1.to_le_bytes());
        out.extend_from_slice(&self.transfer_fee_1.to_le_bytes());
        out.push(u8::from(self.zero_for_one));
        out.extend_from_slice(&self.sqrt_price_x64.to_le_bytes());
        out.extend_from_slice(&self.liquidity.to_le_bytes());
        out.extend_from_slice(&self.tick.to_le_bytes());
        out
    }
}

fn read_exact<const N: usize>(cur: &mut Cursor<&[u8]>) -> anyhow::Result<[u8; N]> {
    let mut buf = [0u8; N];
    cur.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_pubkey_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Pubkey> {
    Ok(Pubkey::new_from_array(read_exact::<32>(cur)?))
}

fn read_u16_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<u16> {
    Ok(u16::from_le_bytes(read_exact::<2>(cur)?))
}

fn read_u64_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<u64> {
    Ok(u64::from_le_bytes(read_exact::<8>(cur)?))
}

fn read_u128_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<u128> {
    Ok(u128::from_le_bytes(read_exact::<16>(cur)?))
}

fn read_i32_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<i32> {
    Ok(i32::from_le_bytes(read_exact::<4>(cur)?))
}

fn read_bool_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<bool> {
    Ok(read_exact::<1>(cur)?[0] != 0)
}

fn extract_invoked_program_id(log: &str) -> Option<&str> {
    const PREFIX: &str = "Program ";
    const INVOKE_SUFFIX: &str = " invoke [";

    if !log.starts_with(PREFIX) {
        return None;
    }
    let rest = &log[PREFIX.len()..];
    let idx = rest.find(INVOKE_SUFFIX)?;
    Some(&rest[..idx])
}

fn is_program_return_log(log: &str) -> bool {
    log.starts_with("Program ") && (log.contains(" success") || log.contains(" failed:"))
}

#[derive(Clone)]
pub struct RaydiumClmm {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
}

impl RaydiumClmm {
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

    fn read_u16(data: &[u8], offset: usize) -> anyhow::Result<u16> {
        let bytes = data
            .get(offset..offset + 2)
            .with_context(|| format!("missing u16 bytes at offset {offset}"))?;
        Ok(u16::from_le_bytes(bytes.try_into()?))
    }

    fn read_i32(data: &[u8], offset: usize) -> anyhow::Result<i32> {
        let bytes = data
            .get(offset..offset + 4)
            .with_context(|| format!("missing i32 bytes at offset {offset}"))?;
        Ok(i32::from_le_bytes(bytes.try_into()?))
    }

    fn read_u128(data: &[u8], offset: usize) -> anyhow::Result<u128> {
        let bytes = data
            .get(offset..offset + 16)
            .with_context(|| format!("missing u128 bytes at offset {offset}"))?;
        Ok(u128::from_le_bytes(bytes.try_into()?))
    }

    pub fn decode_pool_state_account_data(data: &[u8]) -> anyhow::Result<ClmmPoolState> {
        anyhow::ensure!(
            data.len() >= CLMM_POOL_ACCOUNT_LEN,
            "raydium clmm pool account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == POOL_STATE_DISCRIM,
            "raydium clmm pool discriminator mismatch"
        );

        Ok(ClmmPoolState {
            bump: data[8],
            amm_config: Self::read_pubkey(data, 9)?,
            owner: Self::read_pubkey(data, 41)?,
            mint_a: Self::read_pubkey(data, CLMM_POOL_MINT_A_OFFSET)?,
            mint_b: Self::read_pubkey(data, CLMM_POOL_MINT_B_OFFSET)?,
            vault_a: Self::read_pubkey(data, 137)?,
            vault_b: Self::read_pubkey(data, 169)?,
            observation_id: Self::read_pubkey(data, 201)?,
            mint_decimals_a: data[233],
            mint_decimals_b: data[234],
            tick_spacing: Self::read_u16(data, 235)?,
            liquidity: Self::read_u128(data, 237)?,
            sqrt_price_x64: Self::read_u128(data, 253)?,
            tick_current: Self::read_i32(data, 269)?,
            status: data[389],
        })
    }

    fn encode_swap_v2_instruction_data(
        amount: u64,
        other_amount_threshold: u64,
        sqrt_price_limit_x64: u128,
        is_base_input: bool,
    ) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + 8 + 8 + 16 + 1);
        data.extend_from_slice(&SWAP_V2_IX_DISCRIM);
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&other_amount_threshold.to_le_bytes());
        data.extend_from_slice(&sqrt_price_limit_x64.to_le_bytes());
        data.push(u8::from(is_base_input));
        data
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

    fn derive_pool_pda(amm_config: &Pubkey, mint_a: &Pubkey, mint_b: &Pubkey) -> Pubkey {
        let (min_mint, max_mint) = if mint_a.to_bytes() <= mint_b.to_bytes() {
            (mint_a, mint_b)
        } else {
            (mint_b, mint_a)
        };
        Pubkey::find_program_address(
            &[
                POOL_SEED,
                amm_config.as_ref(),
                min_mint.as_ref(),
                max_mint.as_ref(),
            ],
            &RAYDIUM_CLMM_ID,
        )
        .0
    }

    pub fn derive_tick_array(pool: &Pubkey, start_tick_index: i32) -> Pubkey {
        Pubkey::find_program_address(
            &[
                TICK_ARRAY_SEED,
                pool.as_ref(),
                &start_tick_index.to_be_bytes(),
            ],
            &RAYDIUM_CLMM_ID,
        )
        .0
    }

    pub fn derive_ex_bitmap(pool: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[POOL_TICK_ARRAY_BITMAP_SEED, pool.as_ref()],
            &RAYDIUM_CLMM_ID,
        )
        .0
    }

    fn tick_array_start_index(tick_current: i32, tick_spacing: u16) -> i32 {
        let ticks_per_array = i32::from(tick_spacing) * TICK_ARRAY_SIZE;
        let mut compressed = tick_current / ticks_per_array;
        if tick_current < 0 && tick_current % ticks_per_array != 0 {
            compressed -= 1;
        }
        compressed * ticks_per_array
    }

    async fn get_existing_tick_arrays(
        &self,
        pool: &Pubkey,
        state: &ClmmPoolState,
        input_mint: &Pubkey,
    ) -> anyhow::Result<Vec<Pubkey>> {
        let start = Self::tick_array_start_index(state.tick_current, state.tick_spacing);
        let step = i32::from(state.tick_spacing) * TICK_ARRAY_SIZE;
        let zero_for_one = *input_mint == state.mint_a;
        let mut arrays = Vec::with_capacity(DEFAULT_TICK_ARRAY_CANDIDATES);

        for i in 0..DEFAULT_TICK_ARRAY_CANDIDATES {
            let delta = i as i32 * step;
            let start_tick = if zero_for_one {
                start - delta
            } else {
                start + delta
            };
            let pda = Self::derive_tick_array(pool, start_tick);
            if self.account_exists(&pda).await? {
                arrays.push(pda);
            }
        }

        Ok(arrays)
    }

    fn pool_non_wsol_mint(state: &ClmmPoolState) -> Option<Pubkey> {
        if state.mint_a == WSOL_MINT && state.mint_b != WSOL_MINT {
            Some(state.mint_b)
        } else if state.mint_b == WSOL_MINT && state.mint_a != WSOL_MINT {
            Some(state.mint_a)
        } else {
            None
        }
    }

    fn validate_pool_contains_mint(state: &ClmmPoolState, mint: &Pubkey) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.mint_a == *mint || state.mint_b == *mint,
            "raydium clmm pool does not contain mint {}",
            mint
        );
        anyhow::ensure!(
            state.mint_a == WSOL_MINT || state.mint_b == WSOL_MINT,
            "raydium clmm pool is not WSOL-quoted"
        );
        Ok(())
    }

    fn token_decimals_for_mint(state: &ClmmPoolState, mint: &Pubkey) -> anyhow::Result<u8> {
        if state.mint_a == *mint {
            Ok(state.mint_decimals_a)
        } else if state.mint_b == *mint {
            Ok(state.mint_decimals_b)
        } else {
            anyhow::bail!("mint {} not present in pool", mint);
        }
    }

    fn ata_for(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey, sol: &SolHook) -> Pubkey {
        if *token_program == TOKEN_PROGRAM_ID {
            sol.get_ata_for_token(owner, mint)
        } else {
            sol.get_ata_for_token2022(owner, mint)
        }
    }

    fn price_from_sqrt_price_x64_internal(
        sqrt_price_x64: u128,
        mint_decimals_a: u8,
        mint_decimals_b: u8,
    ) -> anyhow::Result<f64> {
        anyhow::ensure!(sqrt_price_x64 > 0, "sqrt_price_x64 must be > 0");
        let sqrt_ratio = sqrt_price_x64 as f64 / 2_f64.powi(64);
        let decimal_adjust = 10_f64.powi(mint_decimals_a as i32 - mint_decimals_b as i32);
        Ok((sqrt_ratio * sqrt_ratio) * decimal_adjust)
    }

    pub fn price_from_sqrt_price_x64(state: &ClmmPoolState) -> anyhow::Result<f64> {
        let token1_per_token0 = Self::price_from_sqrt_price_x64_internal(
            state.sqrt_price_x64,
            state.mint_decimals_a,
            state.mint_decimals_b,
        )?;
        anyhow::ensure!(
            token1_per_token0 > 0.0,
            "invalid clmm price derived from sqrt_price_x64"
        );

        if state.mint_a == WSOL_MINT && state.mint_b != WSOL_MINT {
            Ok(1.0 / token1_per_token0)
        } else if state.mint_b == WSOL_MINT && state.mint_a != WSOL_MINT {
            Ok(token1_per_token0)
        } else {
            Ok(token1_per_token0)
        }
    }

    pub fn parse_logs(
        logs: std::slice::Iter<'_, String>,
        sig: Option<&String>,
    ) -> Vec<RaydiumClmmEvent> {
        let mut events: Vec<RaydiumClmmEvent> = Vec::new();
        let mut invoke_stack: Vec<bool> = Vec::new();
        let mut saw_invoke_frame = false;
        let sig_text = sig.map(String::as_str).unwrap_or("");
        let target_program_mainnet = RAYDIUM_CLMM_ID.to_string();
        let target_program_devnet = RAYDIUM_CLMM_DEVNET_ID.to_string();

        for log in logs {
            if let Some(program_id) = extract_invoked_program_id(log) {
                saw_invoke_frame = true;
                invoke_stack.push(
                    program_id == target_program_mainnet || program_id == target_program_devnet,
                );
                continue;
            }
            if is_program_return_log(log) {
                let _ = invoke_stack.pop();
                continue;
            }
            if !log.contains(SEARCH_FOR) {
                continue;
            }
            if saw_invoke_frame && !invoke_stack.last().copied().unwrap_or(false) {
                continue;
            }
            let (_, payload) = log.split_at(SEARCH_FOR.len());
            let b64 = match decode_b64(payload) {
                Ok(b64) => b64,
                Err(_) => continue,
            };
            if b64.len() < 8 {
                continue;
            }

            if b64[..8] == POOL_CREATED_EVENT_DISCRIM {
                let mut cursor = Cursor::new(&b64[8..]);
                match PoolCreatedEvent::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(RaydiumClmmEvent::PoolCreated(Some(event))),
                    Err(e) => warn!(
                        "Error deserializing raydium clmm pool-created event {:?}: {e}",
                        sig_text
                    ),
                }
            } else if b64[..8] == SWAP_EVENT_DISCRIM {
                let mut cursor = Cursor::new(&b64[8..]);
                match SwapEvent::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(RaydiumClmmEvent::Swap(Some(event))),
                    Err(e) => warn!(
                        "Error deserializing raydium clmm swap event {:?}: {e}",
                        sig_text
                    ),
                }
            } else {
                events.push(RaydiumClmmEvent::Unknown);
            }
        }
        events
    }

    pub async fn fetch_state(&self, pool: &Pubkey) -> anyhow::Result<ClmmPoolState> {
        let data = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("raydium clmm pool account not found"))?
            .data;
        Self::decode_pool_state_account_data(&data)
    }

    pub async fn fetch_price(&self, pool: &Pubkey) -> anyhow::Result<(ClmmPoolState, f64)> {
        let state = self.fetch_state(pool).await?;
        let price = Self::price_from_sqrt_price_x64(&state)?;
        Ok((state, price))
    }

    pub async fn get_mint_from_pool(&self, pool: &Pubkey) -> anyhow::Result<Pubkey> {
        let state = self.fetch_state(pool).await?;
        if let Some(mint) = Self::pool_non_wsol_mint(&state) {
            return Ok(mint);
        }
        Ok(state.mint_a)
    }

    pub async fn find_pools_by_mint(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        if let Some(quote) = quote_mint {
            let mut pools = Vec::new();
            for amm_config in AMM_CONFIGS {
                let pool = Self::derive_pool_pda(&amm_config, mint, quote);
                if self.account_exists(&pool).await? {
                    pools.push(pool);
                }
            }
            return Ok(pools);
        }

        let mut filters_a = vec![
            RpcFilterType::DataSize(CLMM_POOL_ACCOUNT_LEN as u64),
            RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                CLMM_POOL_MINT_A_OFFSET,
                mint.as_ref(),
            )),
        ];
        let mut filters_b = vec![
            RpcFilterType::DataSize(CLMM_POOL_ACCOUNT_LEN as u64),
            RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                CLMM_POOL_MINT_B_OFFSET,
                mint.as_ref(),
            )),
        ];

        let cfg_a = RpcProgramAccountsConfig {
            filters: Some(std::mem::take(&mut filters_a)),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };
        let cfg_b = RpcProgramAccountsConfig {
            filters: Some(std::mem::take(&mut filters_b)),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };

        let accounts_a = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&RAYDIUM_CLMM_ID, cfg_a)
            .await?;
        let accounts_b = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&RAYDIUM_CLMM_ID, cfg_b)
            .await?;

        let mut pools = BTreeSet::new();
        for (pool, _) in accounts_a.into_iter().chain(accounts_b.into_iter()) {
            pools.insert(pool);
        }
        Ok(pools.into_iter().collect())
    }

    pub async fn find_pools_by_owner(&self, owner: &Pubkey) -> anyhow::Result<Vec<Pubkey>> {
        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(CLMM_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    CLMM_POOL_OWNER_OFFSET,
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

        let accounts = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&RAYDIUM_CLMM_ID, cfg)
            .await?;

        Ok(accounts.into_iter().map(|(pool, _)| pool).collect())
    }

    pub async fn find_pool_by_mint_with_min_liquidity(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
        min_liquidity: u128,
    ) -> anyhow::Result<Option<Pubkey>> {
        let pools = self.find_pools_by_mint(mint, quote_mint).await?;
        let mut best_pool = None;
        let mut best_liquidity = 0u128;
        for pool in pools {
            let state = match self.fetch_state(&pool).await {
                Ok(state) => state,
                Err(_) => continue,
            };
            if state.liquidity >= min_liquidity && state.liquidity >= best_liquidity {
                best_liquidity = state.liquidity;
                best_pool = Some(pool);
            }
        }
        Ok(best_pool)
    }

    fn is_pool_input_mint_a(state: &ClmmPoolState, input_mint: &Pubkey) -> anyhow::Result<bool> {
        if state.mint_a == *input_mint {
            Ok(true)
        } else if state.mint_b == *input_mint {
            Ok(false)
        } else {
            anyhow::bail!("input mint {} not found in pool", input_mint)
        }
    }

    async fn build_remaining_accounts(
        &self,
        pool: &Pubkey,
        state: &ClmmPoolState,
        input_mint: &Pubkey,
    ) -> anyhow::Result<Vec<AccountMeta>> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();

        let ex_bitmap = Self::derive_ex_bitmap(pool);
        if self.account_exists(&ex_bitmap).await? && seen.insert(ex_bitmap) {
            out.push(AccountMeta::new(ex_bitmap, false));
        }

        let tick_arrays = self
            .get_existing_tick_arrays(pool, state, input_mint)
            .await?;
        anyhow::ensure!(
            !tick_arrays.is_empty(),
            "no tick arrays found for pool {}",
            pool
        );
        for tick_array in tick_arrays {
            if seen.insert(tick_array) {
                out.push(AccountMeta::new(tick_array, false));
            }
        }
        Ok(out)
    }

    async fn user_token_balance_raw(&self, owner: &Pubkey, mint: &Pubkey) -> anyhow::Result<u64> {
        let token_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .with_context(|| format!("failed to resolve token program for mint {}", mint))?;
        let ata = Self::ata_for(owner, mint, &token_program, &self.sol);
        let balance = self
            .sol
            .rpc_client
            .get_token_account_balance_with_commitment(&ata, CommitmentConfig::confirmed())
            .await
            .with_context(|| format!("failed to fetch token account balance for {}", ata))?;
        Ok(balance.value.amount.parse::<u64>()?)
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
        anyhow::ensure!(price > 0.0, "raydium clmm buy price must be > 0");
        anyhow::ensure!(sol_amount_in > 0.0, "raydium clmm buy amount must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch raydium clmm pool state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;

        let output_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .context("failed to resolve output token program for raydium clmm buy")?;
        let input_program = TOKEN_PROGRAM_ID;

        let input_ata = Self::ata_for(&buyer, &WSOL_MINT, &input_program, &self.sol);
        let output_ata = Self::ata_for(&buyer, mint, &output_program, &self.sol);

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
        anyhow::ensure!(amount_in > 0, "raydium clmm buy amount is too small");

        let mint_decimals = Self::token_decimals_for_mint(&state, mint)?;
        let expected_tokens_out_ui = sol_amount_in / price;
        let min_amount_out = ((expected_tokens_out_ui * (1.0 - slippage_pct)).max(0.0)
            * 10_f64.powi(mint_decimals as i32))
        .floor() as u64;
        let min_amount_out = min_amount_out.max(1);

        ixs.push(system_instruction_if::transfer(
            &buyer, &input_ata, amount_in,
        ));
        ixs.push(sync_native(&TOKEN_PROGRAM_ID, &input_ata)?);

        let input_is_a = Self::is_pool_input_mint_a(&state, &WSOL_MINT)?;
        let (input_vault, output_vault, input_vault_mint, output_vault_mint) = if input_is_a {
            (state.vault_a, state.vault_b, state.mint_a, state.mint_b)
        } else {
            (state.vault_b, state.vault_a, state.mint_b, state.mint_a)
        };

        let mut accounts = vec![
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(state.amm_config, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(input_vault, false),
            AccountMeta::new(output_vault, false),
            AccountMeta::new(state.observation_id, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(MEMO_PROGRAM_ID, false),
            AccountMeta::new_readonly(input_vault_mint, false),
            AccountMeta::new_readonly(output_vault_mint, false),
        ];
        let remaining_accounts = self
            .build_remaining_accounts(pool, &state, &WSOL_MINT)
            .await?;
        accounts.extend(remaining_accounts);

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
            .context("failed to resolve priority fee for raydium clmm buy")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_v2_instruction_data(amount_in, min_amount_out, 0, true);
        ixs.push(Instruction {
            program_id: RAYDIUM_CLMM_ID,
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
        anyhow::ensure!(price > 0.0, "raydium clmm sell price must be > 0");
        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch raydium clmm pool state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;

        let sell_pct = sell_pct.clamp(1, 100);
        let input_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .context("failed to resolve input token program for raydium clmm sell")?;
        let input_ata = Self::ata_for(&buyer, mint, &input_program, &self.sol);
        let output_ata = self.sol.get_ata_for_token(&buyer, &WSOL_MINT);

        let token_balance_raw = self
            .user_token_balance_raw(&buyer, mint)
            .await
            .context("failed to fetch token balance for raydium clmm sell")?;
        anyhow::ensure!(
            token_balance_raw > 0,
            "no token balance for raydium clmm sell"
        );

        let amount_in = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            amount_in > 0,
            "raydium clmm sell amount is too small for requested percentage"
        );

        let slippage_pct = Self::normalize_slippage(slippage);
        let input_decimals = Self::token_decimals_for_mint(&state, mint)?;
        let amount_in_ui = amount_in as f64 / 10_f64.powi(input_decimals as i32);
        let min_sol_output = amount_in_ui * price * (1.0 - slippage_pct);
        let min_sol_output = (min_sol_output.max(0.0) * 1e9).floor() as u64;

        let input_is_a = Self::is_pool_input_mint_a(&state, mint)?;
        let (input_vault, output_vault, input_vault_mint, output_vault_mint) = if input_is_a {
            (state.vault_a, state.vault_b, state.mint_a, state.mint_b)
        } else {
            (state.vault_b, state.vault_a, state.mint_b, state.mint_a)
        };

        let mut ixs = vec![create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &TOKEN_PROGRAM_ID,
        )];

        let mut accounts = vec![
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(state.amm_config, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(input_vault, false),
            AccountMeta::new(output_vault, false),
            AccountMeta::new(state.observation_id, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(MEMO_PROGRAM_ID, false),
            AccountMeta::new_readonly(input_vault_mint, false),
            AccountMeta::new_readonly(output_vault_mint, false),
        ];
        let remaining_accounts = self.build_remaining_accounts(pool, &state, mint).await?;
        accounts.extend(remaining_accounts);

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
            .context("failed to resolve priority fee for raydium clmm sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_v2_instruction_data(amount_in, min_sol_output, 0, true);
        ixs.push(Instruction {
            program_id: RAYDIUM_CLMM_ID,
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
                    .close_token_account_ix(&TOKEN_PROGRAM_ID, &output_ata, &buyer, &buyer)?;
            ixs.push(close_wsol_ix);
        }

        Ok((ixs, recent_fees))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    use solana_program::hash::hash;
    use std::str::FromStr;

    fn encode_fixture_event(discriminator: &[u8; 8], event_payload: &[u8]) -> String {
        let mut payload = Vec::with_capacity(512);
        payload.extend_from_slice(discriminator);
        payload.extend_from_slice(event_payload);
        B64.encode(payload)
    }

    fn anchor_discriminator(namespace: &str, name: &str) -> [u8; 8] {
        let preimage = format!("{namespace}:{name}");
        let digest = hash(preimage.as_bytes()).to_bytes();
        let mut out = [0u8; 8];
        out.copy_from_slice(&digest[..8]);
        out
    }

    #[test]
    fn test_raydium_clmm_discriminators_match_anchor_layout() {
        assert_eq!(
            SWAP_V2_IX_DISCRIM,
            anchor_discriminator("global", "swap_v2")
        );
        assert_eq!(
            POOL_CREATED_EVENT_DISCRIM,
            anchor_discriminator("event", "PoolCreatedEvent")
        );
        assert_eq!(
            SWAP_EVENT_DISCRIM,
            anchor_discriminator("event", "SwapEvent")
        );
        assert_eq!(
            POOL_STATE_DISCRIM,
            anchor_discriminator("account", "PoolState")
        );
    }

    #[test]
    fn test_raydium_clmm_program_constants() {
        let expected_program =
            Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap();
        assert_eq!(RAYDIUM_CLMM_ID, expected_program);
        assert_eq!(CLMM_POOL_ACCOUNT_LEN, 1544);
        assert_eq!(CLMM_POOL_MINT_A_OFFSET, 73);
        assert_eq!(CLMM_POOL_MINT_B_OFFSET, 105);
        assert_eq!(AMM_CONFIGS.len(), 18);
    }

    #[test]
    fn test_raydium_clmm_normalize_slippage() {
        assert_eq!(RaydiumClmm::normalize_slippage(15.0), 0.15);
        assert_eq!(RaydiumClmm::normalize_slippage(0.2), 0.2);
        assert_eq!(RaydiumClmm::normalize_slippage(0.0), 0.01);
        assert_eq!(RaydiumClmm::normalize_slippage(120.0), 0.99);
    }

    #[test]
    fn test_raydium_clmm_encode_swap_instruction_data() {
        let data = RaydiumClmm::encode_swap_v2_instruction_data(1_234, 9_876, 77, true);
        assert_eq!(&data[..8], &SWAP_V2_IX_DISCRIM);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 1_234);
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 9_876);
        assert_eq!(u128::from_le_bytes(data[24..40].try_into().unwrap()), 77);
        assert_eq!(data[40], 1);
        assert_eq!(data.len(), 41);
    }

    #[test]
    fn test_raydium_clmm_decode_pool_state_layout() {
        let mut data = vec![0u8; CLMM_POOL_ACCOUNT_LEN];
        data[..8].copy_from_slice(&POOL_STATE_DISCRIM);
        data[8] = 7; // bump

        let amm = Pubkey::from_str("9iFER3bpjf1PTTCQCfTRu17EJgvsxo9pVyA9QWwEuX4x").unwrap();
        let owner = Pubkey::from_str("11111111111111111111111111111111").unwrap();
        let mint_a = WSOL_MINT;
        let mint_b = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let vault_a = Pubkey::new_unique();
        let vault_b = Pubkey::new_unique();
        let observation = Pubkey::new_unique();

        data[9..41].copy_from_slice(amm.as_ref());
        data[41..73].copy_from_slice(owner.as_ref());
        data[CLMM_POOL_MINT_A_OFFSET..CLMM_POOL_MINT_A_OFFSET + 32]
            .copy_from_slice(mint_a.as_ref());
        data[CLMM_POOL_MINT_B_OFFSET..CLMM_POOL_MINT_B_OFFSET + 32]
            .copy_from_slice(mint_b.as_ref());
        data[137..169].copy_from_slice(vault_a.as_ref());
        data[169..201].copy_from_slice(vault_b.as_ref());
        data[201..233].copy_from_slice(observation.as_ref());
        data[233] = 9;
        data[234] = 6;
        data[235..237].copy_from_slice(&60u16.to_le_bytes());
        data[237..253].copy_from_slice(&123_456u128.to_le_bytes());
        data[253..269].copy_from_slice(&9_999u128.to_le_bytes());
        data[269..273].copy_from_slice(&42i32.to_le_bytes());
        data[389] = 3;

        let state = RaydiumClmm::decode_pool_state_account_data(&data).unwrap();
        assert_eq!(state.bump, 7);
        assert_eq!(state.amm_config, amm);
        assert_eq!(state.owner, owner);
        assert_eq!(state.mint_a, mint_a);
        assert_eq!(state.mint_b, mint_b);
        assert_eq!(state.vault_a, vault_a);
        assert_eq!(state.vault_b, vault_b);
        assert_eq!(state.observation_id, observation);
        assert_eq!(state.mint_decimals_a, 9);
        assert_eq!(state.mint_decimals_b, 6);
        assert_eq!(state.tick_spacing, 60);
        assert_eq!(state.liquidity, 123_456u128);
        assert_eq!(state.sqrt_price_x64, 9_999u128);
        assert_eq!(state.tick_current, 42);
        assert_eq!(state.status, 3);
    }

    #[test]
    fn test_raydium_clmm_price_from_sqrt_price_handles_wsol_pair() {
        let sqrt = ((0.002_f64).sqrt() * 2_f64.powi(64)).round() as u128;
        let state = ClmmPoolState {
            bump: 0,
            amm_config: Pubkey::default(),
            owner: Pubkey::default(),
            mint_a: Pubkey::new_unique(),
            mint_b: WSOL_MINT,
            vault_a: Pubkey::default(),
            vault_b: Pubkey::default(),
            observation_id: Pubkey::default(),
            mint_decimals_a: 9,
            mint_decimals_b: 9,
            tick_spacing: 1,
            liquidity: 1,
            sqrt_price_x64: sqrt,
            tick_current: 0,
            status: 0,
        };

        let price = RaydiumClmm::price_from_sqrt_price_x64(&state).unwrap();
        assert!((price - 0.002).abs() < 1e-9);
    }

    #[test]
    fn test_raydium_clmm_parse_logs_ignores_invalid_payloads() {
        let logs = vec![
            "Program log: hello".to_string(),
            "Program data: not-base64".to_string(),
            format!("Program data: {}", B64.encode([1u8, 2, 3, 4])),
        ];
        let events = RaydiumClmm::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_raydium_clmm_parse_logs_unknown_discriminator() {
        let payload = [1u8, 1, 1, 1, 1, 1, 1, 1, 99];
        let logs = vec![format!("Program data: {}", B64.encode(payload))];
        let events = RaydiumClmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], RaydiumClmmEvent::Unknown));
    }

    #[test]
    fn test_raydium_clmm_parse_logs_pool_created_fixture_decodes() {
        let fixture = PoolCreatedEvent {
            token_mint_0: WSOL_MINT,
            token_mint_1: Pubkey::new_unique(),
            tick_spacing: 60,
            pool_state: Pubkey::new_unique(),
            sqrt_price_x64: 123,
            tick: 10,
            token_vault_0: Pubkey::new_unique(),
            token_vault_1: Pubkey::new_unique(),
        };
        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&POOL_CREATED_EVENT_DISCRIM, &fixture.to_bytes())
        )];
        let events = RaydiumClmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumClmmEvent::PoolCreated(Some(event)) => {
                assert_eq!(event.token_mint_0, fixture.token_mint_0);
                assert_eq!(event.token_mint_1, fixture.token_mint_1);
                assert_eq!(event.pool_state, fixture.pool_state);
                assert_eq!(event.tick_spacing, fixture.tick_spacing);
            }
            _ => panic!("expected parsed raydium clmm pool-created event"),
        }
    }

    #[test]
    fn test_raydium_clmm_parse_logs_swap_fixture_decodes() {
        let fixture = SwapEvent {
            pool_state: Pubkey::new_unique(),
            sender: Pubkey::new_unique(),
            token_account_0: Pubkey::new_unique(),
            token_account_1: Pubkey::new_unique(),
            amount_0: 1_000,
            transfer_fee_0: 10,
            amount_1: 2_000,
            transfer_fee_1: 20,
            zero_for_one: true,
            sqrt_price_x64: 9_999,
            liquidity: 8_888,
            tick: -120,
        };
        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&SWAP_EVENT_DISCRIM, &fixture.to_bytes())
        )];
        let events = RaydiumClmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumClmmEvent::Swap(Some(event)) => {
                assert_eq!(event.pool_state, fixture.pool_state);
                assert_eq!(event.amount_0, fixture.amount_0);
                assert_eq!(event.amount_1, fixture.amount_1);
                assert_eq!(event.tick, fixture.tick);
                assert!(event.zero_for_one);
            }
            _ => panic!("expected parsed raydium clmm swap event"),
        }
    }

    #[test]
    fn test_raydium_clmm_parse_logs_scopes_to_clmm_program_frame() {
        let fixture = SwapEvent {
            pool_state: Pubkey::new_unique(),
            sender: Pubkey::new_unique(),
            token_account_0: Pubkey::new_unique(),
            token_account_1: Pubkey::new_unique(),
            amount_0: 1_000,
            transfer_fee_0: 10,
            amount_1: 2_000,
            transfer_fee_1: 20,
            zero_for_one: true,
            sqrt_price_x64: 9_999,
            liquidity: 8_888,
            tick: -120,
        };

        let other_program = Pubkey::new_unique();
        let logs = vec![
            format!("Program {} invoke [1]", other_program),
            format!(
                "Program data: {}",
                encode_fixture_event(&SWAP_EVENT_DISCRIM, &[1u8, 2, 3, 4])
            ),
            format!("Program {} success", other_program),
            format!("Program {} invoke [1]", RAYDIUM_CLMM_ID),
            format!(
                "Program data: {}",
                encode_fixture_event(&SWAP_EVENT_DISCRIM, &fixture.to_bytes())
            ),
            format!("Program {} success", RAYDIUM_CLMM_ID),
        ];

        let events = RaydiumClmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumClmmEvent::Swap(Some(event)) => {
                assert_eq!(event.pool_state, fixture.pool_state);
                assert_eq!(event.amount_0, fixture.amount_0);
            }
            _ => panic!("expected scoped raydium clmm swap event"),
        }
    }
}
