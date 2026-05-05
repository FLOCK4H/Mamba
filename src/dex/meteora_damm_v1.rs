use crate::core::{
    cluster::{DEFAULT_DEVNET_HTTP_URL, DEFAULT_MAINNET_HTTP_URL, SolanaCluster},
    sol::{
        DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SolHook,
        TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
    },
};
use crate::utils::utils::decode_b64;
use crate::utils::writing::cc;
use crate::{log, warn};
use anyhow::Context;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_message::{VersionedMessage, v0::Message as V0Message};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
};
use solana_rpc_client_types::config::RpcSimulateTransactionConfig;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_system_interface::instruction as system_instruction_if;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status::{
    EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiInstruction, UiMessage,
    UiParsedInstruction, option_serializer::OptionSerializer,
};
use spl_associated_token_account::instruction::{
    create_associated_token_account, create_associated_token_account_idempotent,
};
use spl_token::state::Account as SplTokenAccount;
use spl_token_2022::instruction::sync_native;
use spl_token_2022::state::Account as SplToken2022Account;
use std::io::Cursor;
use std::io::Read;
use std::{collections::BTreeSet, str::FromStr, sync::Arc, time::Duration};

pub const METEORA_DAMM_V1_ID: Pubkey =
    Pubkey::from_str_const("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB");
pub const METEORA_DYNAMIC_VAULT_ID: Pubkey =
    Pubkey::from_str_const("24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi");
pub const METEORA_DYNAMIC_VAULT_BASE_ID: Pubkey =
    Pubkey::from_str_const("HWzXGcGHy4tcpYfaRDCyLNzXqBTv3E6BttpCH2vJxArv");

pub const SWAP_IX_DISCRIM: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
pub const INITIALIZE_POOL_WITH_FEE_TIER_IX_DISCRIM: [u8; 8] = [6, 135, 68, 147, 229, 82, 169, 113];
pub const POOL_DISCRIM: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
pub const POOL_CREATED_EVENT_DISCRIM: [u8; 8] = [202, 44, 41, 88, 104, 220, 157, 82];
pub const SWAP_EVENT_DISCRIM: [u8; 8] = [81, 108, 227, 190, 205, 208, 10, 196];

pub const SEARCH_FOR: &str = "Program data: ";
pub const VAULT_SEED: &[u8] = b"vault";
pub const TOKEN_VAULT_SEED: &[u8] = b"token_vault";
pub const LP_MINT_SEED: &[u8] = b"lp_mint";

pub const DAMM_V1_POOL_ACCOUNT_LEN: usize = 1387;
pub const DAMM_V1_POOL_MIN_DECODE_LEN: usize = 306;
pub const DAMM_V1_POOL_TOKEN_A_MINT_OFFSET: usize = 40;
pub const DAMM_V1_POOL_TOKEN_B_MINT_OFFSET: usize = 72;
pub const DAMM_V1_POOL_A_VAULT_OFFSET: usize = 104;
pub const DAMM_V1_POOL_B_VAULT_OFFSET: usize = 136;
pub const DAMM_V1_POOL_A_VAULT_LP_OFFSET: usize = 168;
pub const DAMM_V1_POOL_B_VAULT_LP_OFFSET: usize = 200;
pub const DAMM_V1_POOL_A_VAULT_LP_BUMP_OFFSET: usize = 232;
pub const DAMM_V1_POOL_ENABLED_OFFSET: usize = 233;
pub const DAMM_V1_POOL_PROTOCOL_TOKEN_A_FEE_OFFSET: usize = 234;
pub const DAMM_V1_POOL_PROTOCOL_TOKEN_B_FEE_OFFSET: usize = 266;
pub const DAMM_V1_POOL_FEE_LAST_UPDATED_AT_OFFSET: usize = 298;

pub const DYNAMIC_VAULT_LP_MINT_OFFSET: usize = 115;
pub const DYNAMIC_VAULT_LP_MINT_REQUIRED_LEN: usize = 147;

#[derive(Debug, Clone)]
pub struct DammV1PoolState {
    pub lp_mint: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub a_vault: Pubkey,
    pub b_vault: Pubkey,
    pub a_vault_lp: Pubkey,
    pub b_vault_lp: Pubkey,
    pub a_vault_lp_bump: u8,
    pub enabled: bool,
    pub protocol_token_a_fee: Pubkey,
    pub protocol_token_b_fee: Pubkey,
    pub fee_last_updated_at: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PoolCreatedEvent {
    pub lp_mint: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub pool_type: u8,
    pub pool: Pubkey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwapEvent {
    pub in_amount: u64,
    pub out_amount: u64,
    pub trade_fee: u64,
    pub protocol_fee: u64,
    pub host_fee: u64,
}

#[derive(Debug)]
pub enum MeteoraDammV1Event {
    PoolCreated(Option<PoolCreatedEvent>),
    Swap(Option<SwapEvent>),
    Unknown,
}

#[derive(Debug, Clone)]
struct SwapVaultAccounts {
    a_token_vault: Pubkey,
    b_token_vault: Pubkey,
    a_vault_lp_mint: Pubkey,
    b_vault_lp_mint: Pubkey,
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

impl PoolCreatedEvent {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        Ok(Self {
            lp_mint: read_pubkey_cursor(cur)?,
            token_a_mint: read_pubkey_cursor(cur)?,
            token_b_mint: read_pubkey_cursor(cur)?,
            pool_type: read_exact::<1>(cur)?[0],
            pool: read_pubkey_cursor(cur)?,
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(129);
        out.extend_from_slice(self.lp_mint.as_ref());
        out.extend_from_slice(self.token_a_mint.as_ref());
        out.extend_from_slice(self.token_b_mint.as_ref());
        out.push(self.pool_type);
        out.extend_from_slice(self.pool.as_ref());
        out
    }
}

impl SwapEvent {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        Ok(Self {
            in_amount: read_u64_cursor(cur)?,
            out_amount: read_u64_cursor(cur)?,
            trade_fee: read_u64_cursor(cur)?,
            protocol_fee: read_u64_cursor(cur)?,
            host_fee: read_u64_cursor(cur)?,
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(40);
        out.extend_from_slice(&self.in_amount.to_le_bytes());
        out.extend_from_slice(&self.out_amount.to_le_bytes());
        out.extend_from_slice(&self.trade_fee.to_le_bytes());
        out.extend_from_slice(&self.protocol_fee.to_le_bytes());
        out.extend_from_slice(&self.host_fee.to_le_bytes());
        out
    }
}

#[derive(Clone)]
pub struct MeteoraDammV1 {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
}

impl MeteoraDammV1 {
    pub fn new(keypair: Arc<Keypair>, sol: Arc<SolHook>) -> Self {
        Self { keypair, sol }
    }

    fn readonly_fallback_rpc_url(&self) -> Option<&'static str> {
        let current_url = self.sol.rpc_client.url();
        let fallback = match self.sol.cluster {
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

    fn decode_pool_account_data(data: &[u8]) -> anyhow::Result<DammV1PoolState> {
        anyhow::ensure!(
            data.len() >= DAMM_V1_POOL_MIN_DECODE_LEN,
            "meteora damm v1 pool account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == POOL_DISCRIM,
            "meteora damm v1 pool discriminator mismatch"
        );

        Ok(DammV1PoolState {
            lp_mint: Self::read_pubkey(data, 8)?,
            token_a_mint: Self::read_pubkey(data, DAMM_V1_POOL_TOKEN_A_MINT_OFFSET)?,
            token_b_mint: Self::read_pubkey(data, DAMM_V1_POOL_TOKEN_B_MINT_OFFSET)?,
            a_vault: Self::read_pubkey(data, DAMM_V1_POOL_A_VAULT_OFFSET)?,
            b_vault: Self::read_pubkey(data, DAMM_V1_POOL_B_VAULT_OFFSET)?,
            a_vault_lp: Self::read_pubkey(data, DAMM_V1_POOL_A_VAULT_LP_OFFSET)?,
            b_vault_lp: Self::read_pubkey(data, DAMM_V1_POOL_B_VAULT_LP_OFFSET)?,
            a_vault_lp_bump: data[DAMM_V1_POOL_A_VAULT_LP_BUMP_OFFSET],
            enabled: data[DAMM_V1_POOL_ENABLED_OFFSET] != 0,
            protocol_token_a_fee: Self::read_pubkey(
                data,
                DAMM_V1_POOL_PROTOCOL_TOKEN_A_FEE_OFFSET,
            )?,
            protocol_token_b_fee: Self::read_pubkey(
                data,
                DAMM_V1_POOL_PROTOCOL_TOKEN_B_FEE_OFFSET,
            )?,
            fee_last_updated_at: Self::read_u64(data, DAMM_V1_POOL_FEE_LAST_UPDATED_AT_OFFSET)?,
        })
    }

    fn encode_swap_instruction_data(amount_in: u64, min_amount_out: u64) -> Vec<u8> {
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&SWAP_IX_DISCRIM);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        data
    }

    fn min_output_after_slippage(expected_out_raw: u64, slippage_pct: f64) -> u64 {
        ((expected_out_raw as f64) * (1.0 - slippage_pct))
            .max(0.0)
            .floor() as u64
    }

    fn min_sol_output_from_price(
        amount_in_raw: u64,
        mint_decimals: u8,
        price: f64,
        slippage_pct: f64,
    ) -> u64 {
        let amount_in_ui = amount_in_raw as f64 / 10_f64.powi(mint_decimals as i32);
        let min_sol_output = amount_in_ui * price * (1.0 - slippage_pct);
        (min_sol_output.max(0.0) * 1e9).floor() as u64
    }

    async fn simulate_sell_out_amount_raw(
        &self,
        buyer: &Pubkey,
        setup_ixs: &[Instruction],
        swap_accounts: &[AccountMeta],
        amount_in: u64,
    ) -> anyhow::Result<Option<u64>> {
        let mut quote_ixs = setup_ixs.to_vec();
        quote_ixs.push(Instruction {
            program_id: METEORA_DAMM_V1_ID,
            accounts: swap_accounts.to_vec(),
            data: Self::encode_swap_instruction_data(amount_in, 0),
        });

        let (blockhash, _) = self
            .sol
            .rpc_client
            .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
            .await?;
        let message =
            VersionedMessage::V0(V0Message::try_compile(buyer, &quote_ixs, &[], blockhash)?);
        let tx = VersionedTransaction::try_new(message, &[self.keypair.as_ref()])?;
        let sim = self
            .sol
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
            .await?;

        if let Some(err) = sim.value.err {
            log!(
                cc::LIGHT_YELLOW,
                "meteora damm v1 sell quote simulation failed (falling back to reserve-price estimate): {:?}",
                err
            );
            return Ok(None);
        }

        let logs = sim.value.logs.unwrap_or_default();
        let events = Self::parse_logs(logs.iter(), None);
        let out_amount = events.into_iter().rev().find_map(|event| match event {
            MeteoraDammV1Event::Swap(Some(swap)) if swap.out_amount > 0 => Some(swap.out_amount),
            _ => None,
        });

        Ok(out_amount)
    }

    async fn account_exists(&self, pubkey: &Pubkey) -> anyhow::Result<bool> {
        match self
            .sol
            .get_account_with_commitment_resilient(pubkey, CommitmentConfig::processed())
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.to_string().to_ascii_lowercase().contains("not found") => Ok(false),
            Err(error) => Err(error),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn derive_vault_address(mint: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[
                VAULT_SEED,
                mint.as_ref(),
                METEORA_DYNAMIC_VAULT_BASE_ID.as_ref(),
            ],
            &METEORA_DYNAMIC_VAULT_ID,
        )
        .0
    }

    fn derive_token_vault_pda(vault: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[TOKEN_VAULT_SEED, vault.as_ref()],
            &METEORA_DYNAMIC_VAULT_ID,
        )
        .0
    }

    fn derive_vault_lp_mint_pda(vault: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[LP_MINT_SEED, vault.as_ref()], &METEORA_DYNAMIC_VAULT_ID).0
    }

    async fn fetch_vault_lp_mint(&self, vault: &Pubkey) -> anyhow::Result<Pubkey> {
        let account = match self
            .sol
            .get_account_with_commitment_resilient(vault, CommitmentConfig::processed())
            .await
        {
            Ok(account) => account,
            Err(error) if error.to_string().to_ascii_lowercase().contains("not found") => {
                return Ok(Self::derive_vault_lp_mint_pda(vault));
            }
            Err(error) => return Err(error),
        };

        if account.owner == METEORA_DYNAMIC_VAULT_ID
            && account.data.len() >= DYNAMIC_VAULT_LP_MINT_REQUIRED_LEN
        {
            return Self::read_pubkey(&account.data, DYNAMIC_VAULT_LP_MINT_OFFSET);
        }

        Ok(Self::derive_vault_lp_mint_pda(vault))
    }

    fn derive_pool_token_vaults(state: &DammV1PoolState) -> (Pubkey, Pubkey) {
        (
            Self::derive_token_vault_pda(&state.a_vault),
            Self::derive_token_vault_pda(&state.b_vault),
        )
    }

    async fn resolve_swap_vault_accounts(
        &self,
        state: &DammV1PoolState,
    ) -> anyhow::Result<SwapVaultAccounts> {
        let (a_token_vault, b_token_vault) = Self::derive_pool_token_vaults(state);
        let a_vault_lp_mint = self.fetch_vault_lp_mint(&state.a_vault).await?;
        let b_vault_lp_mint = self.fetch_vault_lp_mint(&state.b_vault).await?;

        Ok(SwapVaultAccounts {
            a_token_vault,
            b_token_vault,
            a_vault_lp_mint,
            b_vault_lp_mint,
        })
    }

    async fn fetch_token_balance_raw(&self, token_account: &Pubkey) -> anyhow::Result<u64> {
        self.sol
            .get_token_balance_raw_from_ata(token_account)
            .await
            .map(|(raw, _)| raw)
            .with_context(|| {
                format!(
                    "failed to fetch token account balance for {}",
                    token_account
                )
            })
    }

    async fn fetch_reserve_raw(&self, state: &DammV1PoolState) -> anyhow::Result<(u64, u64)> {
        let (a_token_vault, b_token_vault) = Self::derive_pool_token_vaults(state);
        let (reserve_a_raw, reserve_b_raw) = tokio::try_join!(
            self.fetch_token_balance_raw(&a_token_vault),
            self.fetch_token_balance_raw(&b_token_vault)
        )?;
        Ok((reserve_a_raw, reserve_b_raw))
    }

    async fn ensure_supported_pool_tokens(&self, state: &DammV1PoolState) -> anyhow::Result<()> {
        let token_a_program = self
            .sol
            .get_token_program_id(&state.token_a_mint)
            .await
            .with_context(|| {
                format!(
                    "failed to resolve token program for DAMM v1 token_a mint {}",
                    state.token_a_mint
                )
            })?;
        let token_b_program = self
            .sol
            .get_token_program_id(&state.token_b_mint)
            .await
            .with_context(|| {
                format!(
                    "failed to resolve token program for DAMM v1 token_b mint {}",
                    state.token_b_mint
                )
            })?;

        anyhow::ensure!(
            token_a_program == TOKEN_PROGRAM_ID && token_b_program == TOKEN_PROGRAM_ID,
            "meteora damm v1 only supports token-program pools (token_a_program={}, token_b_program={})",
            token_a_program,
            token_b_program
        );

        Ok(())
    }

    fn pool_non_wsol_mint(state: &DammV1PoolState) -> Option<Pubkey> {
        if state.token_a_mint == WSOL_MINT && state.token_b_mint != WSOL_MINT {
            Some(state.token_b_mint)
        } else if state.token_b_mint == WSOL_MINT && state.token_a_mint != WSOL_MINT {
            Some(state.token_a_mint)
        } else {
            None
        }
    }

    fn validate_pool_contains_mint(state: &DammV1PoolState, mint: &Pubkey) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.token_a_mint == *mint || state.token_b_mint == *mint,
            "meteora damm v1 pool does not contain mint {}",
            mint
        );
        anyhow::ensure!(
            state.token_a_mint == WSOL_MINT || state.token_b_mint == WSOL_MINT,
            "meteora damm v1 pool is not WSOL-quoted"
        );
        Ok(())
    }

    async fn user_token_balance_raw(&self, owner: &Pubkey, mint: &Pubkey) -> anyhow::Result<u64> {
        let ata = self.sol.get_ata_for_token(owner, mint);
        self.fetch_token_balance_raw(&ata).await
    }

    pub async fn fetch_wsol_liquidity_raw(&self, state: &DammV1PoolState) -> anyhow::Result<u64> {
        if state.token_a_mint == WSOL_MINT {
            let (a_token_vault, _) = Self::derive_pool_token_vaults(state);
            self.fetch_token_balance_raw(&a_token_vault).await
        } else if state.token_b_mint == WSOL_MINT {
            let (_, b_token_vault) = Self::derive_pool_token_vaults(state);
            self.fetch_token_balance_raw(&b_token_vault).await
        } else {
            anyhow::bail!("pool is not WSOL-quoted")
        }
    }

    pub fn price_from_state_with_reserves_and_decimals(
        state: &DammV1PoolState,
        reserve_a_raw: u64,
        reserve_b_raw: u64,
        decimals_a: u8,
        decimals_b: u8,
    ) -> anyhow::Result<f64> {
        anyhow::ensure!(
            reserve_a_raw > 0 && reserve_b_raw > 0,
            "meteora damm v1 pool has no liquidity"
        );

        let reserve_a = reserve_a_raw as f64 / 10_f64.powi(decimals_a as i32);
        let reserve_b = reserve_b_raw as f64 / 10_f64.powi(decimals_b as i32);

        let (sol_reserve, token_reserve) = if state.token_a_mint == WSOL_MINT {
            (reserve_a, reserve_b)
        } else if state.token_b_mint == WSOL_MINT {
            (reserve_b, reserve_a)
        } else {
            anyhow::bail!("meteora damm v1 pool is not WSOL-quoted")
        };

        anyhow::ensure!(token_reserve > 0.0, "invalid meteora damm v1 token reserve");
        let price = sol_reserve / token_reserve;
        anyhow::ensure!(
            price.is_finite() && price > 0.0,
            "invalid meteora damm v1 token price"
        );
        Ok(price)
    }

    pub fn parse_logs(
        logs: std::slice::Iter<'_, String>,
        sig: Option<&String>,
    ) -> Vec<MeteoraDammV1Event> {
        let mut events: Vec<MeteoraDammV1Event> = Vec::new();
        for log in logs {
            if !log.contains(SEARCH_FOR) {
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
                    Ok(event) => events.push(MeteoraDammV1Event::PoolCreated(Some(event))),
                    Err(e) => warn!(
                        "Error deserializing meteora damm v1 pool-created event {:?}: {e}",
                        sig.unwrap_or(&"".to_string())
                    ),
                }
            } else if b64[..8] == SWAP_EVENT_DISCRIM {
                let mut cursor = Cursor::new(&b64[8..]);
                match SwapEvent::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(MeteoraDammV1Event::Swap(Some(event))),
                    Err(e) => warn!(
                        "Error deserializing meteora damm v1 swap event {:?}: {e}",
                        sig.unwrap_or(&"".to_string())
                    ),
                }
            } else {
                events.push(MeteoraDammV1Event::Unknown);
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
                        .first()
                        .and_then(|v| Pubkey::from_str(v).ok())
                }
                UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed)) => {
                    if parsed.program_id != program_id {
                        return None;
                    }

                    let info = parsed.parsed.get("info")?;
                    for key in ["pool"] {
                        if let Some(value) = info.get(key).and_then(|v| v.as_str())
                            && let Ok(pool) = Pubkey::from_str(value)
                        {
                            return Some(pool);
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
                    let first_account_idx = *compiled.accounts.first()? as usize;
                    let account = account_keys.get(first_account_idx)?;
                    Pubkey::from_str(account).ok()
                }
            }
        }

        let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
            return None;
        };
        let program_id = METEORA_DAMM_V1_ID.to_string();

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
                        .get(1)
                        .and_then(|v| Pubkey::from_str(v).ok())
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let src_idx = *compiled.accounts.get(1)? as usize;
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
        let program_id = METEORA_DAMM_V1_ID.to_string();

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
            .get_account_with_commitment_resilient(&user_source, CommitmentConfig::confirmed())
            .await
            .with_context(|| format!("user source token account {} not found", user_source))?;

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

    pub async fn fetch_state(&self, pool: &Pubkey) -> anyhow::Result<DammV1PoolState> {
        match Self::fetch_state_with_rpc_client(self.sol.rpc_client.as_ref(), pool).await {
            Ok(state) => Ok(state),
            Err(primary_error) => {
                let Some(rpc_url) = self.readonly_fallback_rpc_url() else {
                    return Err(primary_error);
                };
                warn!(
                    "meteora damm v1 fetch_state primary rpc failed for pool {}: {}; retrying via {}",
                    pool, primary_error, rpc_url
                );
                let rpc_client = RpcClient::new_with_commitment(
                    rpc_url.to_string(),
                    CommitmentConfig::confirmed(),
                );
                Self::fetch_state_with_rpc_client(&rpc_client, pool)
                    .await
                    .with_context(|| {
                        format!(
                            "readonly fallback fetch_state failed via {} after primary error: {}",
                            rpc_url, primary_error
                        )
                    })
            }
        }
    }

    async fn fetch_state_with_rpc_client(
        rpc_client: &RpcClient,
        pool: &Pubkey,
    ) -> anyhow::Result<DammV1PoolState> {
        let data = rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("meteora damm v1 pool account not found"))?
            .data;
        Self::decode_pool_account_data(&data)
    }

    pub async fn fetch_price(&self, pool: &Pubkey) -> anyhow::Result<(DammV1PoolState, f64)> {
        let state = self.fetch_state(pool).await?;
        let (reserve_a_raw, reserve_b_raw) = self.fetch_reserve_raw(&state).await?;
        let (decimals_a, decimals_b) = tokio::try_join!(
            async {
                self.sol
                    .get_token_decimals(&state.token_a_mint)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to fetch token-a decimals for mint {}",
                            state.token_a_mint
                        )
                    })
            },
            async {
                self.sol
                    .get_token_decimals(&state.token_b_mint)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to fetch token-b decimals for mint {}",
                            state.token_b_mint
                        )
                    })
            }
        )?;
        let price = Self::price_from_state_with_reserves_and_decimals(
            &state,
            reserve_a_raw,
            reserve_b_raw,
            decimals_a,
            decimals_b,
        )?;

        Ok((state, price))
    }

    pub async fn get_mint_from_pool(&self, pool: &Pubkey) -> anyhow::Result<Pubkey> {
        let state = self.fetch_state(pool).await?;
        if let Some(mint) = Self::pool_non_wsol_mint(&state) {
            return Ok(mint);
        }
        Ok(state.token_a_mint)
    }

    pub async fn find_pools_by_mint(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        match Self::find_pools_by_mint_with_rpc_client(
            self.sol.rpc_client.as_ref(),
            mint,
            quote_mint,
        )
        .await
        {
            Ok(pools) => Ok(pools),
            Err(primary_error) => {
                let Some(rpc_url) = self.readonly_fallback_rpc_url() else {
                    return Err(primary_error);
                };
                warn!(
                    "meteora damm v1 find_pools_by_mint primary rpc failed for mint {}: {}; retrying via {}",
                    mint, primary_error, rpc_url
                );
                let rpc_client = RpcClient::new_with_commitment(
                    rpc_url.to_string(),
                    CommitmentConfig::confirmed(),
                );
                Self::find_pools_by_mint_with_rpc_client(&rpc_client, mint, quote_mint)
                    .await
                    .with_context(|| {
                        format!(
                            "readonly fallback find_pools_by_mint failed via {} after primary error: {}",
                            rpc_url, primary_error
                        )
                    })
            }
        }
    }

    async fn find_pools_by_mint_with_rpc_client(
        rpc_client: &RpcClient,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        if let Some(quote) = quote_mint {
            let cfg_a = RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        DAMM_V1_POOL_TOKEN_A_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        DAMM_V1_POOL_TOKEN_B_MINT_OFFSET,
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
            let cfg_b = RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        DAMM_V1_POOL_TOKEN_B_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        DAMM_V1_POOL_TOKEN_A_MINT_OFFSET,
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

            let pools_a = rpc_client
                .get_program_ui_accounts_with_config(&METEORA_DAMM_V1_ID, cfg_a)
                .await?;
            let pools_b = rpc_client
                .get_program_ui_accounts_with_config(&METEORA_DAMM_V1_ID, cfg_b)
                .await?;

            let mut out = BTreeSet::new();
            for (pool, _) in pools_a.into_iter().chain(pools_b.into_iter()) {
                out.insert(pool);
            }
            return Ok(out.into_iter().collect());
        }

        let cfg_a = RpcProgramAccountsConfig {
            filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                DAMM_V1_POOL_TOKEN_A_MINT_OFFSET,
                mint.as_ref(),
            ))]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };
        let cfg_b = RpcProgramAccountsConfig {
            filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                DAMM_V1_POOL_TOKEN_B_MINT_OFFSET,
                mint.as_ref(),
            ))]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };

        let pools_a = rpc_client
            .get_program_ui_accounts_with_config(&METEORA_DAMM_V1_ID, cfg_a)
            .await?;
        let pools_b = rpc_client
            .get_program_ui_accounts_with_config(&METEORA_DAMM_V1_ID, cfg_b)
            .await?;

        let mut out = BTreeSet::new();
        for (pool, _) in pools_a.into_iter().chain(pools_b.into_iter()) {
            out.insert(pool);
        }

        Ok(out.into_iter().collect())
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
            let liq = match self.fetch_wsol_liquidity_raw(&state).await {
                Ok(liq) => liq,
                Err(_) => continue,
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
        anyhow::ensure!(price > 0.0, "meteora damm v1 buy price must be > 0");
        anyhow::ensure!(
            sol_amount_in > 0.0,
            "meteora damm v1 buy amount must be > 0"
        );

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch meteora damm v1 pool state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;
        self.ensure_supported_pool_tokens(&state).await?;

        anyhow::ensure!(state.enabled, "meteora damm v1 pool is disabled");

        let input_ata = self.sol.get_ata_for_token(&buyer, &WSOL_MINT);
        let output_ata = self.sol.get_ata_for_token(&buyer, mint);

        // DAMM v1 buys routinely reuse an existing WSOL ATA from prior trades or
        // cleanup failures. Default to idempotent ATA creation so those buys do
        // not fail before the swap with ATA-program IllegalOwner on reuse.
        let use_idempotent = use_idempotent.unwrap_or(true);
        let mut ixs = Vec::new();
        if use_idempotent {
            ixs.push(create_associated_token_account_idempotent(
                &buyer,
                &buyer,
                mint,
                &TOKEN_PROGRAM_ID,
            ));
            ixs.push(create_associated_token_account_idempotent(
                &buyer,
                &buyer,
                &WSOL_MINT,
                &TOKEN_PROGRAM_ID,
            ));
        } else {
            ixs.push(create_associated_token_account(
                &buyer,
                &buyer,
                mint,
                &TOKEN_PROGRAM_ID,
            ));
            ixs.push(create_associated_token_account(
                &buyer,
                &buyer,
                &WSOL_MINT,
                &TOKEN_PROGRAM_ID,
            ));
        }

        let slippage_pct = Self::normalize_slippage(slippage);
        let amount_in = (sol_amount_in * 1e9).round() as u64;
        anyhow::ensure!(amount_in > 0, "meteora damm v1 buy amount is too small");

        let mint_decimals = self
            .sol
            .get_token_decimals(mint)
            .await
            .with_context(|| format!("failed to resolve decimals for mint {}", mint))?;
        let expected_tokens_out_ui = sol_amount_in / price;
        let min_amount_out = ((expected_tokens_out_ui * (1.0 - slippage_pct)).max(0.0)
            * 10_f64.powi(mint_decimals as i32))
        .floor() as u64;
        let min_amount_out = min_amount_out.max(1);

        ixs.push(system_instruction_if::transfer(
            &buyer, &input_ata, amount_in,
        ));
        ixs.push(sync_native(&TOKEN_PROGRAM_ID, &input_ata)?);

        let swap_vault_accounts = self.resolve_swap_vault_accounts(&state).await?;
        let protocol_token_fee = if state.token_a_mint == WSOL_MINT {
            state.protocol_token_a_fee
        } else {
            state.protocol_token_b_fee
        };

        let accounts = vec![
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(state.a_vault, false),
            AccountMeta::new(state.b_vault, false),
            AccountMeta::new(swap_vault_accounts.a_token_vault, false),
            AccountMeta::new(swap_vault_accounts.b_token_vault, false),
            AccountMeta::new(swap_vault_accounts.a_vault_lp_mint, false),
            AccountMeta::new(swap_vault_accounts.b_vault_lp_mint, false),
            AccountMeta::new(state.a_vault_lp, false),
            AccountMeta::new(state.b_vault_lp, false),
            AccountMeta::new(protocol_token_fee, false),
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(METEORA_DYNAMIC_VAULT_ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ];

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
            .context("failed to resolve priority fee for meteora damm v1 buy")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_instruction_data(amount_in, min_amount_out);
        ixs.push(Instruction {
            program_id: METEORA_DAMM_V1_ID,
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
        anyhow::ensure!(price > 0.0, "meteora damm v1 sell price must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch meteora damm v1 pool state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;
        self.ensure_supported_pool_tokens(&state).await?;

        anyhow::ensure!(state.enabled, "meteora damm v1 pool is disabled");

        let input_ata = self.sol.get_ata_for_token(&buyer, mint);
        let output_ata = self.sol.get_ata_for_token(&buyer, &WSOL_MINT);

        let token_balance_raw = self
            .user_token_balance_raw(&buyer, mint)
            .await
            .context("failed to fetch token balance for meteora damm v1 sell")?;
        anyhow::ensure!(
            token_balance_raw > 0,
            "no token balance for meteora damm v1 sell"
        );

        let sell_pct = sell_pct.clamp(1, 100);
        let amount_in = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            amount_in > 0,
            "meteora damm v1 sell amount is too small for requested percentage"
        );

        let mint_decimals = self
            .sol
            .get_token_decimals(mint)
            .await
            .with_context(|| format!("failed to resolve decimals for mint {}", mint))?;
        let slippage_pct = Self::normalize_slippage(slippage);
        let create_wsol_ata_ix = create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &TOKEN_PROGRAM_ID,
        );
        let mut ixs = vec![create_wsol_ata_ix.clone()];

        let swap_vault_accounts = self.resolve_swap_vault_accounts(&state).await?;
        let protocol_token_fee = if state.token_a_mint == *mint {
            state.protocol_token_a_fee
        } else {
            state.protocol_token_b_fee
        };

        let accounts = vec![
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(state.a_vault, false),
            AccountMeta::new(state.b_vault, false),
            AccountMeta::new(swap_vault_accounts.a_token_vault, false),
            AccountMeta::new(swap_vault_accounts.b_token_vault, false),
            AccountMeta::new(swap_vault_accounts.a_vault_lp_mint, false),
            AccountMeta::new(swap_vault_accounts.b_vault_lp_mint, false),
            AccountMeta::new(state.a_vault_lp, false),
            AccountMeta::new(state.b_vault_lp, false),
            AccountMeta::new(protocol_token_fee, false),
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(METEORA_DYNAMIC_VAULT_ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ];

        let quoted_sol_output_raw = self
            .simulate_sell_out_amount_raw(&buyer, &[create_wsol_ata_ix], &accounts, amount_in)
            .await?;
        let min_sol_output = match quoted_sol_output_raw {
            Some(expected_out_raw) => {
                Self::min_output_after_slippage(expected_out_raw, slippage_pct)
            }
            None => Self::min_sol_output_from_price(amount_in, mint_decimals, price, slippage_pct),
        };

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
            .context("failed to resolve priority fee for meteora damm v1 sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_instruction_data(amount_in, min_sol_output);
        ixs.push(Instruction {
            program_id: METEORA_DAMM_V1_ID,
            accounts,
            data,
        });

        if amount_in == token_balance_raw && self.account_exists(&input_ata).await? {
            let close_input_ix =
                self.sol
                    .close_token_account_ix(&TOKEN_PROGRAM_ID, &input_ata, &buyer, &buyer)?;
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
    use serde::Deserialize;
    use serde_json::json;
    use solana_program::hash::hash;
    use std::str::FromStr;

    fn encode_fixture_event(discriminator: &[u8; 8], event_payload: &[u8]) -> String {
        let mut payload = Vec::with_capacity(256);
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

    #[derive(Debug, Deserialize)]
    struct FixtureAccountFile {
        account: FixtureAccount,
    }

    #[derive(Debug, Deserialize)]
    struct FixtureAccount {
        data: Vec<String>,
        owner: String,
        space: usize,
    }

    #[test]
    fn test_meteora_damm_v1_discriminators_match_anchor_layout() {
        assert_eq!(SWAP_IX_DISCRIM, anchor_discriminator("global", "swap"));
        assert_eq!(POOL_DISCRIM, anchor_discriminator("account", "Pool"));
        assert_eq!(
            POOL_CREATED_EVENT_DISCRIM,
            anchor_discriminator("event", "PoolCreated")
        );
        assert_eq!(SWAP_EVENT_DISCRIM, anchor_discriminator("event", "Swap"));
    }

    #[test]
    fn test_meteora_damm_v1_program_constants() {
        assert_eq!(
            METEORA_DAMM_V1_ID,
            Pubkey::from_str("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB").unwrap()
        );
        assert_eq!(
            METEORA_DYNAMIC_VAULT_ID,
            Pubkey::from_str("24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi").unwrap()
        );
        assert_eq!(
            METEORA_DYNAMIC_VAULT_BASE_ID,
            Pubkey::from_str("HWzXGcGHy4tcpYfaRDCyLNzXqBTv3E6BttpCH2vJxArv").unwrap()
        );
        assert_eq!(DAMM_V1_POOL_ACCOUNT_LEN, 1387);
        assert_eq!(DAMM_V1_POOL_TOKEN_A_MINT_OFFSET, 40);
        assert_eq!(DAMM_V1_POOL_TOKEN_B_MINT_OFFSET, 72);
    }

    #[test]
    fn test_meteora_damm_v1_swap_instruction_data_encoding() {
        let data = MeteoraDammV1::encode_swap_instruction_data(1_234, 9_876);
        assert_eq!(data.len(), 24);
        assert_eq!(&data[..8], &SWAP_IX_DISCRIM);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 1_234);
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 9_876);
    }

    #[test]
    fn test_meteora_damm_v1_normalize_slippage() {
        assert_eq!(MeteoraDammV1::normalize_slippage(15.0), 0.15);
        assert_eq!(MeteoraDammV1::normalize_slippage(0.25), 0.25);
        assert_eq!(MeteoraDammV1::normalize_slippage(0.0), 0.01);
        assert_eq!(MeteoraDammV1::normalize_slippage(120.0), 0.99);
    }

    #[test]
    fn test_meteora_damm_v1_min_output_after_slippage() {
        assert_eq!(MeteoraDammV1::min_output_after_slippage(993, 0.15), 844);
        assert_eq!(MeteoraDammV1::min_output_after_slippage(1, 0.99), 0);
        assert_eq!(MeteoraDammV1::min_output_after_slippage(0, 0.15), 0);
    }

    #[test]
    fn test_meteora_damm_v1_decode_pool_fixture_layout_contract() {
        let fixture_json = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/external/upstreams/damm-v1-sdk/dynamic-amm-quote/tests/fixtures/accounts/HcjZvfeSNJbNkfLD4eEcRBr96AD3w1GpmMppaeRZf7ur.json"
        ));
        let fixture: FixtureAccountFile = serde_json::from_str(fixture_json).unwrap();
        assert_eq!(fixture.account.owner, METEORA_DAMM_V1_ID.to_string());

        let data = B64.decode(&fixture.account.data[0]).unwrap();
        assert_eq!(data.len(), fixture.account.space);

        let state = MeteoraDammV1::decode_pool_account_data(&data).expect("fixture must decode");
        let msol = Pubkey::from_str("mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So").unwrap();
        assert!(
            (state.token_a_mint == msol && state.token_b_mint == WSOL_MINT)
                || (state.token_b_mint == msol && state.token_a_mint == WSOL_MINT)
        );
        assert_eq!(state.a_vault_lp_bump, 255);
        assert!(state.enabled);
        assert_ne!(state.a_vault, Pubkey::default());
        assert_ne!(state.b_vault, Pubkey::default());
        assert_ne!(state.protocol_token_a_fee, Pubkey::default());
        assert_ne!(state.protocol_token_b_fee, Pubkey::default());
    }

    #[test]
    fn test_meteora_damm_v1_parse_logs_ignores_invalid_payloads() {
        let logs = vec![
            "Program data: not_base64".to_string(),
            "Program data: AQI=".to_string(),
        ];
        let events = MeteoraDammV1::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_meteora_damm_v1_parse_logs_unknown_discriminator() {
        let payload = B64.encode([0u8; 32]);
        let logs = vec![format!("Program data: {payload}")];
        let events = MeteoraDammV1::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MeteoraDammV1Event::Unknown));
    }

    #[test]
    fn test_meteora_damm_v1_parse_logs_pool_created_fixture_decodes() {
        let fixture = PoolCreatedEvent {
            lp_mint: Pubkey::new_unique(),
            token_a_mint: Pubkey::new_unique(),
            token_b_mint: WSOL_MINT,
            pool_type: 1,
            pool: Pubkey::new_unique(),
        };
        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&POOL_CREATED_EVENT_DISCRIM, &fixture.to_bytes())
        )];

        let events = MeteoraDammV1::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDammV1Event::PoolCreated(Some(event)) => {
                assert_eq!(event.lp_mint, fixture.lp_mint);
                assert_eq!(event.token_a_mint, fixture.token_a_mint);
                assert_eq!(event.token_b_mint, fixture.token_b_mint);
                assert_eq!(event.pool, fixture.pool);
                assert_eq!(event.pool_type, fixture.pool_type);
            }
            _ => panic!("expected parsed meteora damm v1 pool-created event"),
        }
    }

    #[test]
    fn test_meteora_damm_v1_parse_logs_swap_fixture_decodes() {
        let fixture = SwapEvent {
            in_amount: 1_000,
            out_amount: 2_000,
            trade_fee: 10,
            protocol_fee: 20,
            host_fee: 3,
        };
        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&SWAP_EVENT_DISCRIM, &fixture.to_bytes())
        )];

        let events = MeteoraDammV1::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDammV1Event::Swap(Some(event)) => {
                assert_eq!(event.in_amount, fixture.in_amount);
                assert_eq!(event.out_amount, fixture.out_amount);
                assert_eq!(event.trade_fee, fixture.trade_fee);
                assert_eq!(event.protocol_fee, fixture.protocol_fee);
                assert_eq!(event.host_fee, fixture.host_fee);
            }
            _ => panic!("expected parsed meteora damm v1 swap event"),
        }
    }

    #[test]
    fn test_meteora_damm_v1_extract_pool_from_inner_instruction_fixture() {
        let pool = "HcjZvfeSNJbNkfLD4eEcRBr96AD3w1GpmMppaeRZf7ur";
        let tx_json = json!({
            "slot": 1,
            "transaction": {
                "signatures": [
                    "1111111111111111111111111111111111111111111111111111111111111111"
                ],
                "message": {
                    "accountKeys": [
                        {
                            "pubkey": "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4",
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": pool,
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": METEORA_DAMM_V1_ID.to_string(),
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
                "preBalances": [0, 0, 0],
                "postBalances": [0, 0, 0],
                "innerInstructions": [
                    {
                        "index": 0,
                        "instructions": [
                            {
                                "programId": METEORA_DAMM_V1_ID.to_string(),
                                "accounts": [pool],
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
            MeteoraDammV1::extract_pool_from_transaction(&tx),
            Some(Pubkey::from_str(pool).unwrap())
        );
    }

    #[test]
    fn test_meteora_damm_v1_derive_vault_address_matches_sdk_fixture() {
        let msol = Pubkey::from_str("mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So").unwrap();
        let expected = Pubkey::from_str("8p1VKP45hhqq5iZG5fNGoi7ucme8nFLeChoDWNy7rWFm").unwrap();
        assert_eq!(MeteoraDammV1::derive_vault_address(&msol), expected);
    }
}
