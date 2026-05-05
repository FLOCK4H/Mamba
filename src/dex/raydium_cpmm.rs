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
use std::io::{Cursor, Read};
use std::{collections::BTreeSet, str::FromStr, sync::Arc, time::Duration};

pub const RAYDIUM_CPMM_ID: Pubkey = crate::core::cluster::RAYDIUM_CPMM_PROGRAM_ID_MAINNET;
pub const RAYDIUM_CPMM_DEVNET_ID: Pubkey = crate::core::cluster::RAYDIUM_CPMM_PROGRAM_ID_DEVNET;

pub const SWAP_BASE_INPUT_IX_DISCRIM: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];
pub const POOL_STATE_DISCRIM: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];
pub const AMM_CONFIG_DISCRIM: [u8; 8] = [218, 244, 33, 104, 203, 203, 43, 111];
pub const LP_CHANGE_EVENT_DISCRIM: [u8; 8] = [121, 163, 205, 201, 57, 218, 117, 60];
pub const SWAP_EVENT_DISCRIM: [u8; 8] = [64, 198, 205, 232, 38, 8, 113, 226];

pub const SEARCH_FOR: &str = "Program data: ";
pub const AUTH_SEED: &[u8] = b"vault_and_lp_mint_auth_seed";
pub const POOL_SEED: &[u8] = b"pool";
pub const POOL_LP_MINT_SEED: &[u8] = b"pool_lp_mint";
pub const POOL_VAULT_SEED: &[u8] = b"pool_vault";
pub const OBSERVATION_SEED: &[u8] = b"observation";
pub const MEMO_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

pub const RAYDIUM_CPMM_INITIALIZE_IX_DISCRIM: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];

pub const CPMM_POOL_ACCOUNT_LEN: usize = 637;
pub const CPMM_POOL_AMM_CONFIG_OFFSET: usize = 8;
pub const CPMM_POOL_POOL_CREATOR_OFFSET: usize = 40;
pub const CPMM_POOL_TOKEN_0_VAULT_OFFSET: usize = 72;
pub const CPMM_POOL_TOKEN_1_VAULT_OFFSET: usize = 104;
pub const CPMM_POOL_LP_MINT_OFFSET: usize = 136;
pub const CPMM_POOL_TOKEN_0_MINT_OFFSET: usize = 168;
pub const CPMM_POOL_TOKEN_1_MINT_OFFSET: usize = 200;
pub const CPMM_POOL_TOKEN_0_PROGRAM_OFFSET: usize = 232;
pub const CPMM_POOL_TOKEN_1_PROGRAM_OFFSET: usize = 264;
pub const CPMM_POOL_OBSERVATION_KEY_OFFSET: usize = 296;
pub const CPMM_POOL_AUTH_BUMP_OFFSET: usize = 328;
pub const CPMM_POOL_STATUS_OFFSET: usize = 329;
pub const CPMM_POOL_LP_MINT_DECIMALS_OFFSET: usize = 330;
pub const CPMM_POOL_MINT_0_DECIMALS_OFFSET: usize = 331;
pub const CPMM_POOL_MINT_1_DECIMALS_OFFSET: usize = 332;
pub const CPMM_POOL_LP_SUPPLY_OFFSET: usize = 333;
pub const CPMM_POOL_PROTOCOL_FEES_TOKEN_0_OFFSET: usize = 341;
pub const CPMM_POOL_PROTOCOL_FEES_TOKEN_1_OFFSET: usize = 349;
pub const CPMM_POOL_FUND_FEES_TOKEN_0_OFFSET: usize = 357;
pub const CPMM_POOL_FUND_FEES_TOKEN_1_OFFSET: usize = 365;
pub const CPMM_POOL_OPEN_TIME_OFFSET: usize = 373;
pub const CPMM_POOL_RECENT_EPOCH_OFFSET: usize = 381;
pub const CPMM_POOL_CREATOR_FEE_ON_OFFSET: usize = 389;
pub const CPMM_POOL_ENABLE_CREATOR_FEE_OFFSET: usize = 390;
pub const CPMM_POOL_CREATOR_FEES_TOKEN_0_OFFSET: usize = 397;
pub const CPMM_POOL_CREATOR_FEES_TOKEN_1_OFFSET: usize = 405;

const POOL_STATUS_SWAP_DISABLE_BIT: u8 = 1 << 2;

#[derive(Debug, Clone)]
pub struct RaydiumCpmmPoolState {
    pub amm_config: Pubkey,
    pub pool_creator: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub lp_mint: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub token_0_program: Pubkey,
    pub token_1_program: Pubkey,
    pub observation_key: Pubkey,
    pub auth_bump: u8,
    pub status: u8,
    pub lp_mint_decimals: u8,
    pub mint_0_decimals: u8,
    pub mint_1_decimals: u8,
    pub lp_supply: u64,
    pub protocol_fees_token_0: u64,
    pub protocol_fees_token_1: u64,
    pub fund_fees_token_0: u64,
    pub fund_fees_token_1: u64,
    pub open_time: u64,
    pub recent_epoch: u64,
    pub creator_fee_on: u8,
    pub enable_creator_fee: bool,
    pub creator_fees_token_0: u64,
    pub creator_fees_token_1: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LpChangeEvent {
    pub pool_id: Pubkey,
    pub lp_amount_before: u64,
    pub token_0_vault_before: u64,
    pub token_1_vault_before: u64,
    pub token_0_amount: u64,
    pub token_1_amount: u64,
    pub token_0_transfer_fee: u64,
    pub token_1_transfer_fee: u64,
    pub change_type: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwapEvent {
    pub pool_id: Pubkey,
    pub input_vault_before: u64,
    pub output_vault_before: u64,
    pub input_amount: u64,
    pub output_amount: u64,
    pub input_transfer_fee: u64,
    pub output_transfer_fee: u64,
    pub base_input: bool,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub trade_fee: u64,
    pub creator_fee: u64,
    pub creator_fee_on_input: bool,
}

#[derive(Debug)]
pub enum RaydiumCpmmEvent {
    LpChange(Option<LpChangeEvent>),
    Swap(Option<SwapEvent>),
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

impl LpChangeEvent {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        Ok(Self {
            pool_id: read_pubkey_cursor(cur)?,
            lp_amount_before: read_u64_cursor(cur)?,
            token_0_vault_before: read_u64_cursor(cur)?,
            token_1_vault_before: read_u64_cursor(cur)?,
            token_0_amount: read_u64_cursor(cur)?,
            token_1_amount: read_u64_cursor(cur)?,
            token_0_transfer_fee: read_u64_cursor(cur)?,
            token_1_transfer_fee: read_u64_cursor(cur)?,
            change_type: read_exact::<1>(cur)?[0],
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(89);
        out.extend_from_slice(self.pool_id.as_ref());
        out.extend_from_slice(&self.lp_amount_before.to_le_bytes());
        out.extend_from_slice(&self.token_0_vault_before.to_le_bytes());
        out.extend_from_slice(&self.token_1_vault_before.to_le_bytes());
        out.extend_from_slice(&self.token_0_amount.to_le_bytes());
        out.extend_from_slice(&self.token_1_amount.to_le_bytes());
        out.extend_from_slice(&self.token_0_transfer_fee.to_le_bytes());
        out.extend_from_slice(&self.token_1_transfer_fee.to_le_bytes());
        out.push(self.change_type);
        out
    }
}

impl SwapEvent {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        Ok(Self {
            pool_id: read_pubkey_cursor(cur)?,
            input_vault_before: read_u64_cursor(cur)?,
            output_vault_before: read_u64_cursor(cur)?,
            input_amount: read_u64_cursor(cur)?,
            output_amount: read_u64_cursor(cur)?,
            input_transfer_fee: read_u64_cursor(cur)?,
            output_transfer_fee: read_u64_cursor(cur)?,
            base_input: read_exact::<1>(cur)?[0] != 0,
            input_mint: read_pubkey_cursor(cur)?,
            output_mint: read_pubkey_cursor(cur)?,
            trade_fee: read_u64_cursor(cur)?,
            creator_fee: read_u64_cursor(cur)?,
            creator_fee_on_input: read_exact::<1>(cur)?[0] != 0,
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(162);
        out.extend_from_slice(self.pool_id.as_ref());
        out.extend_from_slice(&self.input_vault_before.to_le_bytes());
        out.extend_from_slice(&self.output_vault_before.to_le_bytes());
        out.extend_from_slice(&self.input_amount.to_le_bytes());
        out.extend_from_slice(&self.output_amount.to_le_bytes());
        out.extend_from_slice(&self.input_transfer_fee.to_le_bytes());
        out.extend_from_slice(&self.output_transfer_fee.to_le_bytes());
        out.push(u8::from(self.base_input));
        out.extend_from_slice(self.input_mint.as_ref());
        out.extend_from_slice(self.output_mint.as_ref());
        out.extend_from_slice(&self.trade_fee.to_le_bytes());
        out.extend_from_slice(&self.creator_fee.to_le_bytes());
        out.push(u8::from(self.creator_fee_on_input));
        out
    }
}

#[derive(Clone)]
pub struct RaydiumCpmm {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
}

impl RaydiumCpmm {
    pub fn new(keypair: Arc<Keypair>, sol: Arc<SolHook>) -> Self {
        Self { keypair, sol }
    }

    fn program_id(&self) -> Pubkey {
        match self.sol.cluster {
            crate::core::cluster::SolanaCluster::Devnet => RAYDIUM_CPMM_DEVNET_ID,
            _ => RAYDIUM_CPMM_ID,
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

    fn decode_pool_state_account_data(data: &[u8]) -> anyhow::Result<RaydiumCpmmPoolState> {
        anyhow::ensure!(
            data.len() >= CPMM_POOL_ACCOUNT_LEN,
            "raydium cpmm pool account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == POOL_STATE_DISCRIM,
            "raydium cpmm pool discriminator mismatch"
        );

        Ok(RaydiumCpmmPoolState {
            amm_config: Self::read_pubkey(data, CPMM_POOL_AMM_CONFIG_OFFSET)?,
            pool_creator: Self::read_pubkey(data, CPMM_POOL_POOL_CREATOR_OFFSET)?,
            token_0_vault: Self::read_pubkey(data, CPMM_POOL_TOKEN_0_VAULT_OFFSET)?,
            token_1_vault: Self::read_pubkey(data, CPMM_POOL_TOKEN_1_VAULT_OFFSET)?,
            lp_mint: Self::read_pubkey(data, CPMM_POOL_LP_MINT_OFFSET)?,
            token_0_mint: Self::read_pubkey(data, CPMM_POOL_TOKEN_0_MINT_OFFSET)?,
            token_1_mint: Self::read_pubkey(data, CPMM_POOL_TOKEN_1_MINT_OFFSET)?,
            token_0_program: Self::read_pubkey(data, CPMM_POOL_TOKEN_0_PROGRAM_OFFSET)?,
            token_1_program: Self::read_pubkey(data, CPMM_POOL_TOKEN_1_PROGRAM_OFFSET)?,
            observation_key: Self::read_pubkey(data, CPMM_POOL_OBSERVATION_KEY_OFFSET)?,
            auth_bump: data[CPMM_POOL_AUTH_BUMP_OFFSET],
            status: data[CPMM_POOL_STATUS_OFFSET],
            lp_mint_decimals: data[CPMM_POOL_LP_MINT_DECIMALS_OFFSET],
            mint_0_decimals: data[CPMM_POOL_MINT_0_DECIMALS_OFFSET],
            mint_1_decimals: data[CPMM_POOL_MINT_1_DECIMALS_OFFSET],
            lp_supply: Self::read_u64(data, CPMM_POOL_LP_SUPPLY_OFFSET)?,
            protocol_fees_token_0: Self::read_u64(data, CPMM_POOL_PROTOCOL_FEES_TOKEN_0_OFFSET)?,
            protocol_fees_token_1: Self::read_u64(data, CPMM_POOL_PROTOCOL_FEES_TOKEN_1_OFFSET)?,
            fund_fees_token_0: Self::read_u64(data, CPMM_POOL_FUND_FEES_TOKEN_0_OFFSET)?,
            fund_fees_token_1: Self::read_u64(data, CPMM_POOL_FUND_FEES_TOKEN_1_OFFSET)?,
            open_time: Self::read_u64(data, CPMM_POOL_OPEN_TIME_OFFSET)?,
            recent_epoch: Self::read_u64(data, CPMM_POOL_RECENT_EPOCH_OFFSET)?,
            creator_fee_on: data[CPMM_POOL_CREATOR_FEE_ON_OFFSET],
            enable_creator_fee: data[CPMM_POOL_ENABLE_CREATOR_FEE_OFFSET] != 0,
            creator_fees_token_0: Self::read_u64(data, CPMM_POOL_CREATOR_FEES_TOKEN_0_OFFSET)?,
            creator_fees_token_1: Self::read_u64(data, CPMM_POOL_CREATOR_FEES_TOKEN_1_OFFSET)?,
        })
    }

    fn encode_swap_base_input_instruction_data(amount_in: u64, minimum_amount_out: u64) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + 8 + 8);
        data.extend_from_slice(&SWAP_BASE_INPUT_IX_DISCRIM);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&minimum_amount_out.to_le_bytes());
        data
    }

    fn derive_authority_pda() -> Pubkey {
        Self::derive_authority_pda_for(&RAYDIUM_CPMM_ID)
    }

    fn derive_authority_pda_for(program_id: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[AUTH_SEED], program_id).0
    }

    fn ata_for(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(owner, mint, token_program)
    }

    fn pool_non_wsol_mint(state: &RaydiumCpmmPoolState) -> Option<Pubkey> {
        if state.token_0_mint == WSOL_MINT && state.token_1_mint != WSOL_MINT {
            Some(state.token_1_mint)
        } else if state.token_1_mint == WSOL_MINT && state.token_0_mint != WSOL_MINT {
            Some(state.token_0_mint)
        } else {
            None
        }
    }

    fn token_decimals_for_mint(state: &RaydiumCpmmPoolState, mint: &Pubkey) -> anyhow::Result<u8> {
        if state.token_0_mint == *mint {
            Ok(state.mint_0_decimals)
        } else if state.token_1_mint == *mint {
            Ok(state.mint_1_decimals)
        } else {
            anyhow::bail!("mint {} not present in pool", mint)
        }
    }

    fn token_program_for_mint(
        state: &RaydiumCpmmPoolState,
        mint: &Pubkey,
    ) -> anyhow::Result<Pubkey> {
        if state.token_0_mint == *mint {
            Ok(state.token_0_program)
        } else if state.token_1_mint == *mint {
            Ok(state.token_1_program)
        } else {
            anyhow::bail!("mint {} not present in pool", mint)
        }
    }

    fn validate_pool_contains_mint(
        state: &RaydiumCpmmPoolState,
        mint: &Pubkey,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.token_0_mint == *mint || state.token_1_mint == *mint,
            "raydium cpmm pool does not contain mint {}",
            mint
        );
        anyhow::ensure!(
            state.token_0_mint == WSOL_MINT || state.token_1_mint == WSOL_MINT,
            "raydium cpmm pool is not WSOL-quoted"
        );
        Ok(())
    }

    fn validate_swap_enabled(state: &RaydiumCpmmPoolState) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.status & POOL_STATUS_SWAP_DISABLE_BIT == 0,
            "raydium cpmm pool swap is disabled (status={})",
            state.status
        );
        Ok(())
    }

    fn vault_net_amounts(
        state: &RaydiumCpmmPoolState,
        vault_0_raw: u64,
        vault_1_raw: u64,
    ) -> (u64, u64) {
        let fees_0 = state
            .protocol_fees_token_0
            .saturating_add(state.fund_fees_token_0)
            .saturating_add(state.creator_fees_token_0);
        let fees_1 = state
            .protocol_fees_token_1
            .saturating_add(state.fund_fees_token_1)
            .saturating_add(state.creator_fees_token_1);
        (
            vault_0_raw.saturating_sub(fees_0),
            vault_1_raw.saturating_sub(fees_1),
        )
    }

    fn sol_price_from_vault_amounts(
        state: &RaydiumCpmmPoolState,
        vault_0_net: u64,
        vault_1_net: u64,
    ) -> anyhow::Result<f64> {
        if state.token_0_mint == WSOL_MINT && state.token_1_mint != WSOL_MINT {
            let sol_reserve = vault_0_net as f64 / 1e9;
            let token_reserve = vault_1_net as f64 / 10_f64.powi(state.mint_1_decimals as i32);
            anyhow::ensure!(token_reserve > 0.0, "raydium cpmm token reserve is zero");
            let price = sol_reserve / token_reserve;
            anyhow::ensure!(
                price.is_finite() && price > 0.0,
                "invalid raydium cpmm price"
            );
            return Ok(price);
        }
        if state.token_1_mint == WSOL_MINT && state.token_0_mint != WSOL_MINT {
            let sol_reserve = vault_1_net as f64 / 1e9;
            let token_reserve = vault_0_net as f64 / 10_f64.powi(state.mint_0_decimals as i32);
            anyhow::ensure!(token_reserve > 0.0, "raydium cpmm token reserve is zero");
            let price = sol_reserve / token_reserve;
            anyhow::ensure!(
                price.is_finite() && price > 0.0,
                "invalid raydium cpmm price"
            );
            return Ok(price);
        }
        anyhow::bail!("raydium cpmm pool is not WSOL-quoted");
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
        state: &RaydiumCpmmPoolState,
    ) -> anyhow::Result<(u64, u64)> {
        let vault_0_raw = self.token_balance_raw(&state.token_0_vault).await?;
        let vault_1_raw = self.token_balance_raw(&state.token_1_vault).await?;
        Ok((vault_0_raw, vault_1_raw))
    }

    async fn user_token_balance_raw(&self, owner: &Pubkey, mint: &Pubkey) -> anyhow::Result<u64> {
        let token_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .with_context(|| format!("failed to resolve token program for mint {}", mint))?;
        let ata = Self::ata_for(owner, mint, &token_program);
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

    fn swap_vaults_for_input(
        state: &RaydiumCpmmPoolState,
        input_mint: &Pubkey,
    ) -> anyhow::Result<(Pubkey, Pubkey, Pubkey, Pubkey, u8, u8)> {
        if state.token_0_mint == *input_mint {
            Ok((
                state.token_0_vault,
                state.token_1_vault,
                state.token_0_mint,
                state.token_1_mint,
                state.mint_0_decimals,
                state.mint_1_decimals,
            ))
        } else if state.token_1_mint == *input_mint {
            Ok((
                state.token_1_vault,
                state.token_0_vault,
                state.token_1_mint,
                state.token_0_mint,
                state.mint_1_decimals,
                state.mint_0_decimals,
            ))
        } else {
            anyhow::bail!("input mint {} not found in pool", input_mint)
        }
    }

    pub fn parse_logs(
        logs: std::slice::Iter<'_, String>,
        sig: Option<&String>,
    ) -> Vec<RaydiumCpmmEvent> {
        let mut events = Vec::new();
        let mut invoke_stack: Vec<bool> = Vec::new();
        let mut saw_invoke_frame = false;
        let sig_text = sig.map(String::as_str).unwrap_or("");
        let target_program_mainnet = RAYDIUM_CPMM_ID.to_string();
        let target_program_devnet = RAYDIUM_CPMM_DEVNET_ID.to_string();

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
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if b64.len() < 8 {
                continue;
            }

            if b64[..8] == LP_CHANGE_EVENT_DISCRIM {
                let mut cursor = Cursor::new(&b64[8..]);
                match LpChangeEvent::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(RaydiumCpmmEvent::LpChange(Some(event))),
                    Err(err) => warn!(
                        "Error deserializing raydium cpmm lp-change event {:?}: {err}",
                        sig_text
                    ),
                }
            } else if b64[..8] == SWAP_EVENT_DISCRIM {
                let mut cursor = Cursor::new(&b64[8..]);
                match SwapEvent::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(RaydiumCpmmEvent::Swap(Some(event))),
                    Err(err) => warn!(
                        "Error deserializing raydium cpmm swap event {:?}: {err}",
                        sig_text
                    ),
                }
            } else {
                events.push(RaydiumCpmmEvent::Unknown);
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
                        .get(3)
                        .and_then(|value| Pubkey::from_str(value).ok())
                }
                UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed)) => {
                    if parsed.program_id != program_id {
                        return None;
                    }
                    let info = parsed.parsed.get("info")?;
                    if let Some(pool) = info.get("poolState").and_then(|value| value.as_str()) {
                        return Pubkey::from_str(pool).ok();
                    }
                    if let Some(pool) = info.get("pool_state").and_then(|value| value.as_str()) {
                        return Pubkey::from_str(pool).ok();
                    }
                    if let Some(pool) = info.get("pool").and_then(|value| value.as_str()) {
                        return Pubkey::from_str(pool).ok();
                    }
                    None
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let pool_idx = *compiled.accounts.get(3)? as usize;
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

        let program_id = RAYDIUM_CPMM_ID.to_string();
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
                        .get(4)
                        .and_then(|value| Pubkey::from_str(value).ok())
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let src_idx = *compiled.accounts.get(4)? as usize;
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

        let program_id = RAYDIUM_CPMM_ID.to_string();
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
        if token_account.owner == spl_token_2022::id() {
            let state = SplToken2022Account::unpack(&token_account.data)
                .context("failed to parse token-2022 account state")?;
            return Ok(Some(state.mint));
        }

        Ok(None)
    }

    pub async fn fetch_state(&self, pool: &Pubkey) -> anyhow::Result<RaydiumCpmmPoolState> {
        let data = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("raydium cpmm pool account not found"))?
            .data;

        Self::decode_pool_state_account_data(&data)
    }

    pub async fn fetch_wsol_liquidity_raw(
        &self,
        state: &RaydiumCpmmPoolState,
    ) -> anyhow::Result<u64> {
        let (vault_0_raw, vault_1_raw) = self.fetch_vault_amounts_raw(state).await?;
        let (vault_0_net, vault_1_net) = Self::vault_net_amounts(state, vault_0_raw, vault_1_raw);

        if state.token_0_mint == WSOL_MINT {
            return Ok(vault_0_net);
        }
        if state.token_1_mint == WSOL_MINT {
            return Ok(vault_1_net);
        }
        anyhow::bail!("raydium cpmm pool quote mint is not WSOL")
    }

    pub async fn fetch_price(&self, pool: &Pubkey) -> anyhow::Result<(RaydiumCpmmPoolState, f64)> {
        let state = self.fetch_state(pool).await?;
        let (vault_0_raw, vault_1_raw) = self.fetch_vault_amounts_raw(&state).await?;
        let (vault_0_net, vault_1_net) = Self::vault_net_amounts(&state, vault_0_raw, vault_1_raw);
        let price = Self::sol_price_from_vault_amounts(&state, vault_0_net, vault_1_net)?;
        Ok((state, price))
    }

    pub async fn get_mint_from_pool(&self, pool: &Pubkey) -> anyhow::Result<Pubkey> {
        let state = self.fetch_state(pool).await?;
        if let Some(mint) = Self::pool_non_wsol_mint(&state) {
            return Ok(mint);
        }
        Ok(state.token_0_mint)
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
                    RpcFilterType::DataSize(CPMM_POOL_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        0,
                        POOL_STATE_DISCRIM.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        CPMM_POOL_TOKEN_0_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        CPMM_POOL_TOKEN_1_MINT_OFFSET,
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
                    RpcFilterType::DataSize(CPMM_POOL_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        0,
                        POOL_STATE_DISCRIM.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        CPMM_POOL_TOKEN_1_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        CPMM_POOL_TOKEN_0_MINT_OFFSET,
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

        let cfg_0 = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(CPMM_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, POOL_STATE_DISCRIM.as_ref())),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    CPMM_POOL_TOKEN_0_MINT_OFFSET,
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
        let cfg_1 = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(CPMM_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, POOL_STATE_DISCRIM.as_ref())),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    CPMM_POOL_TOKEN_1_MINT_OFFSET,
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

        Ok(out.into_iter().collect())
    }

    pub async fn find_pools_by_creator(&self, creator: &Pubkey) -> anyhow::Result<Vec<Pubkey>> {
        let program_id = self.program_id();
        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(CPMM_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, POOL_STATE_DISCRIM.as_ref())),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    CPMM_POOL_POOL_CREATOR_OFFSET,
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
                RpcFilterType::DataSize(CPMM_POOL_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(0, POOL_STATE_DISCRIM.as_ref())),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    CPMM_POOL_LP_MINT_OFFSET,
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
    ) -> anyhow::Result<(RaydiumCpmmPoolState, u64, u64)> {
        anyhow::ensure!(
            lp_token_amount > 0,
            "raydium cpmm withdraw lp amount must be > 0"
        );

        let state = self.fetch_state(pool).await?;
        anyhow::ensure!(state.lp_supply > 0, "raydium cpmm pool lp_supply is zero");
        anyhow::ensure!(
            lp_token_amount <= state.lp_supply,
            "raydium cpmm withdraw lp amount exceeds pool lp_supply"
        );

        let (vault_0_raw, vault_1_raw) = self.fetch_vault_amounts_raw(&state).await?;
        let (vault_0_net, vault_1_net) = Self::vault_net_amounts(&state, vault_0_raw, vault_1_raw);

        let token_0_out = ((u128::from(vault_0_net) * u128::from(lp_token_amount))
            / u128::from(state.lp_supply)) as u64;
        let token_1_out = ((u128::from(vault_1_net) * u128::from(lp_token_amount))
            / u128::from(state.lp_supply)) as u64;
        anyhow::ensure!(
            token_0_out > 0 || token_1_out > 0,
            "raydium cpmm withdraw quote resulted in zero outputs"
        );

        Ok((state, token_0_out, token_1_out))
    }

    pub async fn withdraw_for_user(
        &self,
        owner: &Pubkey,
        pool: &Pubkey,
        lp_token_amount: u64,
        minimum_token_0_amount: u64,
        minimum_token_1_amount: u64,
    ) -> anyhow::Result<(Vec<Instruction>, RaydiumCpmmPoolState, u64, u64)> {
        let owner = *owner;
        let program_id = self.program_id();
        let authority = Self::derive_authority_pda_for(&program_id);
        let (state, token_0_out, token_1_out) = self
            .estimate_withdraw_amounts_raw(pool, lp_token_amount)
            .await?;

        let owner_lp_token = Self::ata_for(&owner, &state.lp_mint, &TOKEN_PROGRAM_ID);
        let token_0_account = Self::ata_for(&owner, &state.token_0_mint, &state.token_0_program);
        let token_1_account = Self::ata_for(&owner, &state.token_1_mint, &state.token_1_program);

        let mut instructions = Vec::new();
        instructions.push(create_associated_token_account_idempotent(
            &owner,
            &owner,
            &state.token_0_mint,
            &state.token_0_program,
        ));
        instructions.push(create_associated_token_account_idempotent(
            &owner,
            &owner,
            &state.token_1_mint,
            &state.token_1_program,
        ));

        let mut data = Vec::with_capacity(8 + 8 + 8 + 8);
        data.extend_from_slice(&[183, 18, 70, 156, 148, 109, 161, 34]);
        data.extend_from_slice(&lp_token_amount.to_le_bytes());
        data.extend_from_slice(&minimum_token_0_amount.to_le_bytes());
        data.extend_from_slice(&minimum_token_1_amount.to_le_bytes());

        instructions.push(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(owner, true),
                AccountMeta::new_readonly(authority, false),
                AccountMeta::new(*pool, false),
                AccountMeta::new(owner_lp_token, false),
                AccountMeta::new(token_0_account, false),
                AccountMeta::new(token_1_account, false),
                AccountMeta::new(state.token_0_vault, false),
                AccountMeta::new(state.token_1_vault, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
                AccountMeta::new_readonly(state.token_0_mint, false),
                AccountMeta::new_readonly(state.token_1_mint, false),
                AccountMeta::new(state.lp_mint, false),
                AccountMeta::new_readonly(MEMO_PROGRAM_ID, false),
            ],
            data,
        });

        Ok((instructions, state, token_0_out, token_1_out))
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
        anyhow::ensure!(price > 0.0, "raydium cpmm buy price must be > 0");
        anyhow::ensure!(sol_amount_in > 0.0, "raydium cpmm buy amount must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch raydium cpmm state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;
        Self::validate_swap_enabled(&state)?;

        let output_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .context("failed to resolve output token program for raydium cpmm buy")?;
        let expected_output_program = Self::token_program_for_mint(&state, mint)?;
        anyhow::ensure!(
            output_program == expected_output_program,
            "raydium cpmm output token program mismatch"
        );

        let input_program = TOKEN_PROGRAM_ID;
        let input_ata = Self::ata_for(&buyer, &WSOL_MINT, &input_program);
        let output_ata = Self::ata_for(&buyer, mint, &output_program);

        let use_idempotent = use_idempotent.unwrap_or(true);
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
        anyhow::ensure!(amount_in > 0, "raydium cpmm buy amount is too small");

        let output_decimals = Self::token_decimals_for_mint(&state, mint)?;
        let min_amount_out =
            Self::quote_buy_min_output_raw(sol_amount_in, price, slippage_pct, output_decimals);

        ixs.push(system_instruction_if::transfer(
            &buyer, &input_ata, amount_in,
        ));
        ixs.push(sync_native(&input_program, &input_ata)?);

        let authority = Self::derive_authority_pda();
        let (input_vault, output_vault, input_mint, output_mint, _, _) =
            Self::swap_vaults_for_input(&state, &WSOL_MINT)?;
        let mut accounts = vec![
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new_readonly(state.amm_config, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(input_vault, false),
            AccountMeta::new(output_vault, false),
            AccountMeta::new_readonly(input_program, false),
            AccountMeta::new_readonly(output_program, false),
            AccountMeta::new_readonly(input_mint, false),
            AccountMeta::new_readonly(output_mint, false),
            AccountMeta::new(state.observation_key, false),
        ];

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
            .context("failed to resolve priority fee for raydium cpmm buy")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_base_input_instruction_data(amount_in, min_amount_out);
        ixs.push(Instruction {
            program_id: RAYDIUM_CPMM_ID,
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
        anyhow::ensure!(price > 0.0, "raydium cpmm sell price must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch raydium cpmm state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;
        Self::validate_swap_enabled(&state)?;

        let sell_pct = sell_pct.clamp(1, 100);

        let input_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .context("failed to resolve input token program for raydium cpmm sell")?;
        let expected_input_program = Self::token_program_for_mint(&state, mint)?;
        anyhow::ensure!(
            input_program == expected_input_program,
            "raydium cpmm input token program mismatch"
        );

        let input_ata = Self::ata_for(&buyer, mint, &input_program);
        let output_program = TOKEN_PROGRAM_ID;
        let output_ata = Self::ata_for(&buyer, &WSOL_MINT, &output_program);

        let token_balance_raw = self
            .user_token_balance_raw(&buyer, mint)
            .await
            .context("failed to fetch token balance for raydium cpmm sell")?;
        anyhow::ensure!(
            token_balance_raw > 0,
            "no token balance for raydium cpmm sell"
        );

        let amount_in = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            amount_in > 0,
            "raydium cpmm sell amount is too small for requested percentage"
        );

        let slippage_pct = Self::normalize_slippage(slippage);
        let input_decimals = Self::token_decimals_for_mint(&state, mint)?;
        let min_sol_output =
            Self::quote_sell_min_output_raw(amount_in, input_decimals, price, slippage_pct);

        let authority = Self::derive_authority_pda();
        let (input_vault, output_vault, input_mint, output_mint, _, _) =
            Self::swap_vaults_for_input(&state, mint)?;

        let mut ixs = vec![create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &output_program,
        )];

        let mut accounts = vec![
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new_readonly(state.amm_config, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(input_vault, false),
            AccountMeta::new(output_vault, false),
            AccountMeta::new_readonly(input_program, false),
            AccountMeta::new_readonly(output_program, false),
            AccountMeta::new_readonly(input_mint, false),
            AccountMeta::new_readonly(output_mint, false),
            AccountMeta::new(state.observation_key, false),
        ];

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
            .context("failed to resolve priority fee for raydium cpmm sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_base_input_instruction_data(amount_in, min_sol_output);
        ixs.push(Instruction {
            program_id: RAYDIUM_CPMM_ID,
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
        let mut payload = Vec::with_capacity(512);
        payload.extend_from_slice(discriminator);
        payload.extend_from_slice(event_payload);
        B64.encode(payload)
    }

    fn synthetic_pool_state_account_bytes() -> Vec<u8> {
        let mut data = vec![0u8; CPMM_POOL_ACCOUNT_LEN];
        data[..8].copy_from_slice(&POOL_STATE_DISCRIM);

        let amm_config = Pubkey::new_unique();
        let pool_creator = Pubkey::new_unique();
        let token_0_vault = Pubkey::new_unique();
        let token_1_vault = Pubkey::new_unique();
        let lp_mint = Pubkey::new_unique();
        let token_0_mint = WSOL_MINT;
        let token_1_mint = Pubkey::new_unique();
        let observation_key = Pubkey::new_unique();

        data[CPMM_POOL_AMM_CONFIG_OFFSET..CPMM_POOL_AMM_CONFIG_OFFSET + 32]
            .copy_from_slice(amm_config.as_ref());
        data[CPMM_POOL_POOL_CREATOR_OFFSET..CPMM_POOL_POOL_CREATOR_OFFSET + 32]
            .copy_from_slice(pool_creator.as_ref());
        data[CPMM_POOL_TOKEN_0_VAULT_OFFSET..CPMM_POOL_TOKEN_0_VAULT_OFFSET + 32]
            .copy_from_slice(token_0_vault.as_ref());
        data[CPMM_POOL_TOKEN_1_VAULT_OFFSET..CPMM_POOL_TOKEN_1_VAULT_OFFSET + 32]
            .copy_from_slice(token_1_vault.as_ref());
        data[CPMM_POOL_LP_MINT_OFFSET..CPMM_POOL_LP_MINT_OFFSET + 32]
            .copy_from_slice(lp_mint.as_ref());
        data[CPMM_POOL_TOKEN_0_MINT_OFFSET..CPMM_POOL_TOKEN_0_MINT_OFFSET + 32]
            .copy_from_slice(token_0_mint.as_ref());
        data[CPMM_POOL_TOKEN_1_MINT_OFFSET..CPMM_POOL_TOKEN_1_MINT_OFFSET + 32]
            .copy_from_slice(token_1_mint.as_ref());
        data[CPMM_POOL_TOKEN_0_PROGRAM_OFFSET..CPMM_POOL_TOKEN_0_PROGRAM_OFFSET + 32]
            .copy_from_slice(TOKEN_PROGRAM_ID.as_ref());
        data[CPMM_POOL_TOKEN_1_PROGRAM_OFFSET..CPMM_POOL_TOKEN_1_PROGRAM_OFFSET + 32]
            .copy_from_slice(TOKEN_PROGRAM_ID.as_ref());
        data[CPMM_POOL_OBSERVATION_KEY_OFFSET..CPMM_POOL_OBSERVATION_KEY_OFFSET + 32]
            .copy_from_slice(observation_key.as_ref());

        data[CPMM_POOL_AUTH_BUMP_OFFSET] = 255;
        data[CPMM_POOL_STATUS_OFFSET] = 0;
        data[CPMM_POOL_LP_MINT_DECIMALS_OFFSET] = 9;
        data[CPMM_POOL_MINT_0_DECIMALS_OFFSET] = 9;
        data[CPMM_POOL_MINT_1_DECIMALS_OFFSET] = 6;

        data[CPMM_POOL_LP_SUPPLY_OFFSET..CPMM_POOL_LP_SUPPLY_OFFSET + 8]
            .copy_from_slice(&1_000_000_000u64.to_le_bytes());
        data[CPMM_POOL_PROTOCOL_FEES_TOKEN_0_OFFSET..CPMM_POOL_PROTOCOL_FEES_TOKEN_0_OFFSET + 8]
            .copy_from_slice(&10u64.to_le_bytes());
        data[CPMM_POOL_PROTOCOL_FEES_TOKEN_1_OFFSET..CPMM_POOL_PROTOCOL_FEES_TOKEN_1_OFFSET + 8]
            .copy_from_slice(&20u64.to_le_bytes());
        data[CPMM_POOL_FUND_FEES_TOKEN_0_OFFSET..CPMM_POOL_FUND_FEES_TOKEN_0_OFFSET + 8]
            .copy_from_slice(&3u64.to_le_bytes());
        data[CPMM_POOL_FUND_FEES_TOKEN_1_OFFSET..CPMM_POOL_FUND_FEES_TOKEN_1_OFFSET + 8]
            .copy_from_slice(&4u64.to_le_bytes());
        data[CPMM_POOL_OPEN_TIME_OFFSET..CPMM_POOL_OPEN_TIME_OFFSET + 8]
            .copy_from_slice(&1_700_000_000u64.to_le_bytes());
        data[CPMM_POOL_RECENT_EPOCH_OFFSET..CPMM_POOL_RECENT_EPOCH_OFFSET + 8]
            .copy_from_slice(&42u64.to_le_bytes());
        data[CPMM_POOL_CREATOR_FEE_ON_OFFSET] = 0;
        data[CPMM_POOL_ENABLE_CREATOR_FEE_OFFSET] = 1;
        data[CPMM_POOL_CREATOR_FEES_TOKEN_0_OFFSET..CPMM_POOL_CREATOR_FEES_TOKEN_0_OFFSET + 8]
            .copy_from_slice(&5u64.to_le_bytes());
        data[CPMM_POOL_CREATOR_FEES_TOKEN_1_OFFSET..CPMM_POOL_CREATOR_FEES_TOKEN_1_OFFSET + 8]
            .copy_from_slice(&6u64.to_le_bytes());

        data
    }

    #[test]
    fn test_raydium_cpmm_discriminators_match_anchor_layout() {
        assert_eq!(
            SWAP_BASE_INPUT_IX_DISCRIM,
            anchor_discriminator("global", "swap_base_input")
        );
        assert_eq!(
            POOL_STATE_DISCRIM,
            anchor_discriminator("account", "PoolState")
        );
        assert_eq!(
            AMM_CONFIG_DISCRIM,
            anchor_discriminator("account", "AmmConfig")
        );
        assert_eq!(
            SWAP_EVENT_DISCRIM,
            anchor_discriminator("event", "SwapEvent")
        );
        assert_eq!(
            LP_CHANGE_EVENT_DISCRIM,
            anchor_discriminator("event", "LpChangeEvent")
        );
    }

    #[test]
    fn test_raydium_cpmm_program_constants() {
        assert_eq!(
            RAYDIUM_CPMM_ID,
            Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C").unwrap()
        );
        assert_eq!(CPMM_POOL_ACCOUNT_LEN, 637);
        assert_eq!(CPMM_POOL_TOKEN_0_MINT_OFFSET, 168);
        assert_eq!(CPMM_POOL_TOKEN_1_MINT_OFFSET, 200);
    }

    #[test]
    fn test_raydium_cpmm_normalize_slippage() {
        assert_eq!(RaydiumCpmm::normalize_slippage(15.0), 0.15);
        assert_eq!(RaydiumCpmm::normalize_slippage(0.2), 0.2);
        assert_eq!(RaydiumCpmm::normalize_slippage(0.0), 0.01);
        assert_eq!(RaydiumCpmm::normalize_slippage(120.0), 0.99);
    }

    #[test]
    fn test_raydium_cpmm_encode_swap_instruction_data_layout() {
        let data = RaydiumCpmm::encode_swap_base_input_instruction_data(1_234, 9_876);
        assert_eq!(data.len(), 24);
        assert_eq!(&data[..8], &SWAP_BASE_INPUT_IX_DISCRIM);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 1_234);
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 9_876);
    }

    #[test]
    fn test_raydium_cpmm_decode_pool_state_layout() {
        let data = synthetic_pool_state_account_bytes();
        let state = RaydiumCpmm::decode_pool_state_account_data(&data).unwrap();

        assert_eq!(state.token_0_mint, WSOL_MINT);
        assert_eq!(state.status, 0);
        assert_eq!(state.mint_0_decimals, 9);
        assert_eq!(state.mint_1_decimals, 6);
        assert_eq!(state.protocol_fees_token_0, 10);
        assert_eq!(state.protocol_fees_token_1, 20);
        assert_eq!(state.fund_fees_token_0, 3);
        assert_eq!(state.fund_fees_token_1, 4);
        assert_eq!(state.creator_fees_token_0, 5);
        assert_eq!(state.creator_fees_token_1, 6);
    }

    #[test]
    fn test_raydium_cpmm_sol_price_from_vault_amounts_uses_net_vaults() {
        let state =
            RaydiumCpmm::decode_pool_state_account_data(&synthetic_pool_state_account_bytes())
                .unwrap();
        let price =
            RaydiumCpmm::sol_price_from_vault_amounts(&state, 1_000_000_000, 200_000_000).unwrap();
        assert!((price - 0.00500000066).abs() < 1e-9);
    }

    #[test]
    fn test_raydium_cpmm_parse_logs_ignores_invalid_payloads() {
        let logs = vec![
            "Program log: hello".to_string(),
            "Program data: not-base64".to_string(),
            format!("Program data: {}", B64.encode([1u8, 2, 3, 4])),
        ];
        let events = RaydiumCpmm::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_raydium_cpmm_parse_logs_unknown_event() {
        let payload = [1u8, 1, 1, 1, 1, 1, 1, 1, 99];
        let logs = vec![format!("Program data: {}", B64.encode(payload))];
        let events = RaydiumCpmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], RaydiumCpmmEvent::Unknown));
    }

    #[test]
    fn test_raydium_cpmm_parse_logs_lp_change_fixture_decodes() {
        let fixture = LpChangeEvent {
            pool_id: Pubkey::new_unique(),
            lp_amount_before: 10,
            token_0_vault_before: 11,
            token_1_vault_before: 12,
            token_0_amount: 13,
            token_1_amount: 14,
            token_0_transfer_fee: 15,
            token_1_transfer_fee: 16,
            change_type: 0,
        };

        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&LP_CHANGE_EVENT_DISCRIM, &fixture.to_bytes())
        )];

        let events = RaydiumCpmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumCpmmEvent::LpChange(Some(event)) => {
                assert_eq!(event.pool_id, fixture.pool_id);
                assert_eq!(event.token_0_amount, fixture.token_0_amount);
                assert_eq!(event.token_1_amount, fixture.token_1_amount);
                assert_eq!(event.change_type, fixture.change_type);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_raydium_cpmm_parse_logs_swap_fixture_decodes() {
        let fixture = SwapEvent {
            pool_id: Pubkey::new_unique(),
            input_vault_before: 1_000,
            output_vault_before: 2_000,
            input_amount: 111,
            output_amount: 222,
            input_transfer_fee: 3,
            output_transfer_fee: 4,
            base_input: true,
            input_mint: WSOL_MINT,
            output_mint: Pubkey::new_unique(),
            trade_fee: 7,
            creator_fee: 8,
            creator_fee_on_input: true,
        };

        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&SWAP_EVENT_DISCRIM, &fixture.to_bytes())
        )];

        let events = RaydiumCpmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumCpmmEvent::Swap(Some(event)) => {
                assert_eq!(event.pool_id, fixture.pool_id);
                assert_eq!(event.input_amount, fixture.input_amount);
                assert_eq!(event.output_amount, fixture.output_amount);
                assert_eq!(event.input_mint, fixture.input_mint);
                assert_eq!(event.output_mint, fixture.output_mint);
                assert_eq!(event.creator_fee_on_input, fixture.creator_fee_on_input);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_raydium_cpmm_parse_logs_scopes_to_cpmm_program_frame() {
        let fixture = SwapEvent {
            pool_id: Pubkey::new_unique(),
            input_vault_before: 1_000,
            output_vault_before: 2_000,
            input_amount: 111,
            output_amount: 222,
            input_transfer_fee: 3,
            output_transfer_fee: 4,
            base_input: true,
            input_mint: WSOL_MINT,
            output_mint: Pubkey::new_unique(),
            trade_fee: 7,
            creator_fee: 8,
            creator_fee_on_input: true,
        };

        let other_program = Pubkey::new_unique();
        let logs = vec![
            format!("Program {} invoke [1]", other_program),
            format!(
                "Program data: {}",
                encode_fixture_event(&SWAP_EVENT_DISCRIM, &[1u8, 2, 3, 4])
            ),
            format!("Program {} success", other_program),
            format!("Program {} invoke [1]", RAYDIUM_CPMM_ID),
            format!(
                "Program data: {}",
                encode_fixture_event(&SWAP_EVENT_DISCRIM, &fixture.to_bytes())
            ),
            format!("Program {} success", RAYDIUM_CPMM_ID),
        ];

        let events = RaydiumCpmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RaydiumCpmmEvent::Swap(Some(event)) => {
                assert_eq!(event.pool_id, fixture.pool_id);
                assert_eq!(event.input_amount, fixture.input_amount);
            }
            other => panic!("unexpected event parsed: {:?}", other),
        }
    }

    #[test]
    fn test_raydium_cpmm_extract_pool_from_inner_instruction_fixture() {
        let pool = Pubkey::new_unique();
        let amm_config = Pubkey::new_unique();

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
                        },
                        {
                            "pubkey": RaydiumCpmm::derive_authority_pda().to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": amm_config.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": pool.to_string(),
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": RAYDIUM_CPMM_ID.to_string(),
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
                                "programId": RAYDIUM_CPMM_ID.to_string(),
                                "accounts": [
                                    Pubkey::new_unique().to_string(),
                                    RaydiumCpmm::derive_authority_pda().to_string(),
                                    amm_config.to_string(),
                                    pool.to_string()
                                ],
                                "data": B64.encode(vec![1u8; 24]),
                                "stackHeight": 2
                            }
                        ]
                    }
                ]
            }
        });

        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(tx_json).unwrap();
        assert_eq!(RaydiumCpmm::extract_pool_from_transaction(&tx), Some(pool));
    }
}
