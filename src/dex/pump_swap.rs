use crate::utils::writing::cc;
use crate::{log, warn};
use anyhow::Context;
use {
    crate::core::create::compile_unsigned_v0_transaction,
    anchor_lang::{InstructionData, ToAccountMetas},
    pump_swap_types::state::{GlobalConfig, Pool},
    solana_system_interface::instruction as system_instruction_if,
    spl_token_2022::instruction::sync_native,
};

use tokio::sync::Mutex;
#[allow(unused_imports)]
use {
    crate::core::sol::{
        DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SYSTEM_PROGRAM, SolHook,
        TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
    },
    crate::utils::utils::decode_b64,
    borsh010::BorshDeserialize,
    pump_swap_types::{
        BondingCurve,
        events::{BuyEvent, CreatePoolEvent, DepositEvent, SellEvent, WithdrawEvent},
    },
    solana_address::Address,
    solana_commitment_config::CommitmentConfig,
    solana_keypair::Keypair,
    solana_program::{
        instruction::{AccountMeta, Instruction},
        pubkey::Pubkey,
    },
    solana_rpc_client_types::config::RpcSimulateTransactionConfig,
    solana_signer::Signer,
    spl_associated_token_account::instruction::{
        create_associated_token_account, create_associated_token_account_idempotent,
    },
    std::{collections::HashMap, io::Cursor, sync::Arc},
    tokio,
};

pub const BUY_IX_DISCRIM: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const BUY_EXACT_QUOTE_IN_IX_DISCRIM: [u8; 8] = [198, 46, 21, 82, 180, 217, 232, 112];
pub const SELL_IX_DISCRIM: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

pub const CREATE_POOL_EVENT_DISCRIM: [u8; 8] = [177, 49, 12, 210, 160, 118, 167, 116];
pub const BUY_EVENT_DISCRIM: [u8; 8] = [103, 244, 82, 31, 44, 245, 119, 119];
pub const SELL_EVENT_DISCRIM: [u8; 8] = [62, 47, 55, 10, 165, 3, 220, 42];
pub const DEPOSIT_EVENT_DISCRIM: [u8; 8] = [120, 248, 61, 83, 31, 142, 107, 144];
pub const WITHDRAW_EVENT_DISCRIM: [u8; 8] = [22, 9, 133, 26, 160, 44, 71, 192];
pub const POOL_ACCOUNT_DISCRIM: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
pub const PUMP_SWAP_ID: Pubkey =
    Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
pub const EVENT_AUTHORITY: Pubkey =
    Pubkey::from_str_const("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR");
pub const GLOBAL_CONFIG_PUB: Pubkey =
    Pubkey::from_str_const("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw");
pub const PROTOCOL_FEE_RECIP: Pubkey =
    Pubkey::from_str_const("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV");
pub const ASSOCIATED_TOKEN_PROGRAM: Pubkey =
    Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const GLOBAL_VOLUME_ACCUMULATOR: Pubkey =
    Pubkey::from_str_const("C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw");
pub const FEE_CONFIG: Pubkey =
    Pubkey::from_str_const("5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx");
pub const FEE_PROGRAM: Pubkey =
    Pubkey::from_str_const("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
pub const MAYHEM_FEE_RECIPIENT: Pubkey =
    Pubkey::from_str_const("GesfTA3X2arioaHp8bbKdjG9vJtskViWACZoYvxp4twS");

pub const SEARCH_FOR: &str = "Program data: ";
const FEE_BPS_SCALE: u128 = 10_000;
const BUYBACK_FEE_RECIPIENT_COUNT: usize = 8;

// Offsets for base/quote mints inside PumpSwap Pool account data
pub const CREATOR_OFFSET: usize = 11;
pub const BASE_MINT_OFFSET: usize = 43;
pub const QUOTE_MINT_OFFSET: usize = 75;
pub const LP_MINT_OFFSET: usize = 107;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PoolLookupSpec {
    base_mint: Option<Pubkey>,
    quote_mint: Option<Pubkey>,
}

#[derive(Debug)]
pub enum PumpSwapEvent {
    CreatePool(Option<CreatePoolEvent>),
    Buy(Option<BuyEvent>),
    Sell(Option<SellEvent>),
    Deposit(Option<DepositEvent>),
    Withdraw(Option<WithdrawEvent>),
    Unknown,
}

#[derive(Clone)]
pub struct PumpSwap {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
    pool_state_cache: Arc<Mutex<HashMap<Pubkey, Vec<u8>>>>,
}

impl PumpSwap {
    pub fn new(keypair: Arc<Keypair>, sol: Arc<SolHook>) -> Self {
        Self::new_with_pool_state_cache(keypair, sol, Arc::new(Mutex::new(HashMap::new())))
    }

    pub fn new_with_pool_state_cache(
        keypair: Arc<Keypair>,
        sol: Arc<SolHook>,
        pool_state_cache: Arc<Mutex<HashMap<Pubkey, Vec<u8>>>>,
    ) -> Self {
        Self {
            keypair,
            sol,
            pool_state_cache,
        }
    }

    pub fn shared_pool_state_cache(&self) -> Arc<Mutex<HashMap<Pubkey, Vec<u8>>>> {
        self.pool_state_cache.clone()
    }

    fn normalize_slippage(slippage: f64) -> f64 {
        let normalized = if slippage > 1.0 {
            slippage / 100.0
        } else {
            slippage
        };
        normalized.max(0.0)
    }

    fn encode_buy_instruction_data(
        base_amount_out: u64,
        max_quote_amount_in: u64,
        track_volume: bool,
    ) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + 8 + 8 + 1);
        data.extend_from_slice(&BUY_IX_DISCRIM);
        data.extend_from_slice(&base_amount_out.to_le_bytes());
        data.extend_from_slice(&max_quote_amount_in.to_le_bytes());
        data.push(u8::from(track_volume));
        data
    }

    fn encode_buy_exact_quote_in_instruction_data(
        spendable_quote_in: u64,
        min_base_amount_out: u64,
        track_volume: bool,
    ) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + 8 + 8 + 1);
        data.extend_from_slice(&BUY_EXACT_QUOTE_IN_IX_DISCRIM);
        data.extend_from_slice(&spendable_quote_in.to_le_bytes());
        data.extend_from_slice(&min_base_amount_out.to_le_bytes());
        data.push(u8::from(track_volume));
        data
    }

    fn encode_sell_instruction_data(base_amount_in: u64, min_quote_amount_out: u64) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + 8 + 8);
        data.extend_from_slice(&SELL_IX_DISCRIM);
        data.extend_from_slice(&base_amount_in.to_le_bytes());
        data.extend_from_slice(&min_quote_amount_out.to_le_bytes());
        data
    }

    #[cfg(test)]
    fn apply_buy_slippage(quoted_base_amount_out: u64, slippage_pct: f64) -> u64 {
        if quoted_base_amount_out == 0 {
            return 0;
        }

        ((quoted_base_amount_out as f64) * (1.0 - slippage_pct))
            .max(0.0)
            .floor() as u64
    }

    fn apply_sell_slippage(quoted_quote_amount_out: u64, slippage_pct: f64) -> u64 {
        if quoted_quote_amount_out == 0 {
            return 0;
        }

        ((quoted_quote_amount_out as f64) * (1.0 - slippage_pct))
            .max(0.0)
            .floor() as u64
    }

    fn checked_mul_div_u128(
        lhs: u128,
        rhs: u128,
        divisor: u128,
        label: &str,
    ) -> anyhow::Result<u128> {
        anyhow::ensure!(divisor > 0, "{label} division by zero");
        lhs.checked_mul(rhs)
            .with_context(|| format!("{label} overflow"))?
            .checked_div(divisor)
            .with_context(|| format!("{label} division failed"))
    }

    fn effective_buy_quote_amount_in_raw(
        user_quote_amount_in: u64,
        total_fee_bps: u64,
    ) -> anyhow::Result<u64> {
        if user_quote_amount_in == 0 {
            return Ok(0);
        }

        let denominator = FEE_BPS_SCALE
            .checked_add(u128::from(total_fee_bps))
            .context("pump.swap effective quote denominator overflow")?;
        let effective_quote_amount_in = u128::from(user_quote_amount_in)
            .checked_mul(FEE_BPS_SCALE)
            .context("pump.swap effective quote numerator overflow")?
            .checked_div(denominator)
            .context("pump.swap effective quote division failed")?;
        anyhow::ensure!(
            effective_quote_amount_in <= u128::from(u64::MAX),
            "pump.swap effective quote overflow u64"
        );
        Ok(effective_quote_amount_in as u64)
    }

    fn quote_buy_base_amount_out_with_total_fee_bps(
        user_quote_amount_in: u64,
        base_reserve: u64,
        quote_reserve: u64,
        total_fee_bps: u64,
    ) -> anyhow::Result<u64> {
        if user_quote_amount_in == 0 {
            return Ok(0);
        }

        anyhow::ensure!(
            base_reserve > 0 && quote_reserve > 0,
            "pump.swap reserves are invalid"
        );
        let effective_quote_amount_in =
            Self::effective_buy_quote_amount_in_raw(user_quote_amount_in, total_fee_bps)?;
        anyhow::ensure!(
            effective_quote_amount_in > 0,
            "pump.swap buy amount too small after fees"
        );

        let base_amount_out = Self::checked_mul_div_u128(
            u128::from(effective_quote_amount_in),
            u128::from(base_reserve),
            u128::from(quote_reserve)
                .checked_add(u128::from(effective_quote_amount_in))
                .context("pump.swap buy quote denominator overflow")?,
            "pump.swap buy quote",
        )?;
        let capped = base_amount_out.min(u128::from(base_reserve));
        anyhow::ensure!(capped > 0, "pump.swap buy quote returned zero tokens");
        anyhow::ensure!(
            capped <= u128::from(u64::MAX),
            "pump.swap buy quote overflow u64"
        );
        Ok(capped as u64)
    }

    fn pool_lookup_specs(mint: &Pubkey) -> Vec<PoolLookupSpec> {
        vec![
            PoolLookupSpec {
                base_mint: Some(*mint),
                quote_mint: None,
            },
            PoolLookupSpec {
                base_mint: None,
                quote_mint: Some(*mint),
            },
        ]
    }

    fn token_account_for_pool_mint(state: &Pool, mint: &Pubkey) -> Option<Pubkey> {
        let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
        if base_mint == *mint {
            return Some(Pubkey::new_from_array(
                state.pool_base_token_account.to_bytes(),
            ));
        }

        let quote_mint = Pubkey::new_from_array(state.quote_mint.to_bytes());
        if quote_mint == *mint {
            return Some(Pubkey::new_from_array(
                state.pool_quote_token_account.to_bytes(),
            ));
        }

        None
    }

    fn liquidity_token_account_for_mint_pair(
        state: &Pool,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> Option<Pubkey> {
        let desired_quote_mint = quote_mint.copied().unwrap_or(WSOL_MINT);
        let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
        let state_quote_mint = Pubkey::new_from_array(state.quote_mint.to_bytes());

        if base_mint == *mint && state_quote_mint == desired_quote_mint {
            return Some(Pubkey::new_from_array(
                state.pool_quote_token_account.to_bytes(),
            ));
        }

        if state_quote_mint == *mint && base_mint == desired_quote_mint {
            return Some(Pubkey::new_from_array(
                state.pool_base_token_account.to_bytes(),
            ));
        }

        None
    }

    fn decode_pool_state_bytes(pool: &Pubkey, state: &[u8]) -> anyhow::Result<Pool> {
        anyhow::ensure!(
            state.len() >= 8,
            "pump.swap pool account {} is too small: {} bytes",
            pool,
            state.len()
        );
        anyhow::ensure!(
            state.starts_with(&POOL_ACCOUNT_DISCRIM),
            "pump.swap pool account {} has unexpected discriminator",
            pool
        );
        let mut cursor = Cursor::new(&state[8..]);
        Pool::deserialize_reader(&mut cursor)
            .with_context(|| format!("failed to decode pump.swap pool state for {}", pool))
    }

    async fn cache_pool_state_bytes(&self, pool: &Pubkey, state: &[u8]) {
        self.pool_state_cache
            .lock()
            .await
            .insert(*pool, state.to_vec());
    }

    async fn resolve_pool_mint_token_program(
        &self,
        state: &Pool,
        mint: &Pubkey,
        context: &str,
    ) -> anyhow::Result<Pubkey> {
        match self.sol.get_token_program_id(mint).await {
            Ok(program) if program == TOKEN_PROGRAM_ID || program == TOKEN_2022_PROGRAM_ID => {
                Ok(program)
            }
            Ok(program) => {
                if let Some(token_account) = Self::token_account_for_pool_mint(state, mint)
                    && let Ok(fallback_program) = self
                        .sol
                        .get_token_program_id_for_token_account(&token_account)
                        .await
                {
                    warn!(
                        "resolved {} mint {} token program from pool token account {} after unexpected mint owner {}",
                        context, mint, token_account, program
                    );
                    return Ok(fallback_program);
                }
                anyhow::bail!(
                    "unsupported token program for {} mint {}: {}",
                    context,
                    mint,
                    program
                );
            }
            Err(error) => {
                let token_account =
                    Self::token_account_for_pool_mint(state, mint).with_context(|| {
                        format!("{context} mint {mint} is not present in the selected pool state")
                    })?;
                let fallback_program = self
                    .sol
                    .get_token_program_id_for_token_account(&token_account)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to resolve token program for {} mint {} from pool token account {} after mint lookup error: {}",
                            context,
                            mint,
                            token_account,
                            error
                        )
                    })?;
                warn!(
                    "resolved {} mint {} token program from pool token account {} after mint lookup error: {}",
                    context, mint, token_account, error
                );
                Ok(fallback_program)
            }
        }
    }

    fn decode_fee_bps_from_log_line(log: &str) -> Option<(u64, u64, u64)> {
        let prefix = format!("Program return: {} ", FEE_PROGRAM);
        let payload = log.strip_prefix(&prefix)?;
        let raw = decode_b64(payload).ok()?;
        if raw.len() < 24 {
            return None;
        }
        let lp_fee_bps = u64::from_le_bytes(raw[0..8].try_into().ok()?);
        let protocol_fee_bps = u64::from_le_bytes(raw[8..16].try_into().ok()?);
        let creator_fee_bps = u64::from_le_bytes(raw[16..24].try_into().ok()?);
        Some((lp_fee_bps, protocol_fee_bps, creator_fee_bps))
    }

    async fn simulate_buy_fee_bps(
        &self,
        buyer: &Pubkey,
        setup_instructions: &[Instruction],
        accounts: &[AccountMeta],
        pool: &Pubkey,
        spendable_quote_in: u64,
    ) -> anyhow::Result<(u64, u64, u64)> {
        let mut quote_ixs = setup_instructions.to_vec();
        quote_ixs.push(Instruction {
            program_id: PUMP_SWAP_ID,
            accounts: accounts.to_vec(),
            data: Self::encode_buy_exact_quote_in_instruction_data(spendable_quote_in, 1, true),
        });

        let (blockhash, _) = self
            .sol
            .get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
            .await
            .context("failed to fetch blockhash for pump.swap fee simulation")?;
        let tx = compile_unsigned_v0_transaction(buyer, &quote_ixs, blockhash)
            .context("failed to compile unsigned tx for pump.swap fee simulation")?;
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
            .await
            .context("pump.swap fee simulation rpc call failed")?;
        let logs = sim.value.logs.unwrap_or_default();
        let fees = logs
            .iter()
            .find_map(|log| Self::decode_fee_bps_from_log_line(log))
            .with_context(|| {
                format!(
                    "pump.swap fee simulation missing fee-program return for pool {}",
                    pool
                )
            })?;
        if let Some(err) = sim.value.err {
            warn!(
                "pump.swap fee simulation ended with {:?} for pool {} buyer {}; extracted fee_bps={:?}; logs: {}",
                err,
                pool,
                buyer,
                fees,
                logs.join(" | ")
            );
        }
        Ok(fees)
    }

    async fn quote_buy_base_amount_out_raw(
        &self,
        buyer: &Pubkey,
        state: &Pool,
        setup_instructions: &[Instruction],
        accounts: &[AccountMeta],
        pool: &Pubkey,
        user_quote_amount_in: u64,
    ) -> anyhow::Result<u64> {
        if user_quote_amount_in == 0 {
            return Ok(0);
        }

        let pool_base_token_account =
            Pubkey::new_from_array(state.pool_base_token_account.to_bytes());
        let pool_quote_token_account =
            Pubkey::new_from_array(state.pool_quote_token_account.to_bytes());
        let base_reserve = self
            .token_balance_raw(&pool_base_token_account)
            .await
            .context("failed to fetch pump.swap base reserve for buy quote")?;
        let quote_reserve = self
            .token_balance_raw(&pool_quote_token_account)
            .await
            .context("failed to fetch pump.swap quote reserve for buy quote")?;
        anyhow::ensure!(
            base_reserve > 0 && quote_reserve > 0,
            "pump.swap reserves are invalid"
        );
        let (lp_fee_bps, protocol_fee_bps, creator_fee_bps) = self
            .simulate_buy_fee_bps(
                buyer,
                setup_instructions,
                accounts,
                pool,
                user_quote_amount_in,
            )
            .await
            .context("failed to simulate pump.swap fee bps for buy quote")?;
        let total_fee_bps = lp_fee_bps
            .checked_add(protocol_fee_bps)
            .and_then(|total| total.checked_add(creator_fee_bps))
            .context("pump.swap total fee bps overflow")?;

        let effective_quote_amount_in =
            Self::effective_buy_quote_amount_in_raw(user_quote_amount_in, total_fee_bps)?;
        log!(
            cc::LIGHT_CYAN,
            "pump.swap buy quote spend={} lp_bps={} protocol_bps={} creator_bps={} total_bps={} effective_quote={}",
            user_quote_amount_in,
            lp_fee_bps,
            protocol_fee_bps,
            creator_fee_bps,
            total_fee_bps,
            effective_quote_amount_in
        );

        Self::quote_buy_base_amount_out_with_total_fee_bps(
            user_quote_amount_in,
            base_reserve,
            quote_reserve,
            total_fee_bps,
        )
    }

    async fn simulate_buy_output_raw(
        &self,
        buyer: &Pubkey,
        instructions: &[Instruction],
        pool: &Pubkey,
        expected_ix_name: &str,
    ) -> anyhow::Result<u64> {
        let (blockhash, _) = self
            .sol
            .get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
            .await
            .context("failed to fetch blockhash for pump.swap buy quote simulation")?;
        let tx = compile_unsigned_v0_transaction(buyer, instructions, blockhash)
            .context("failed to compile unsigned tx for pump.swap buy quote simulation")?;
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
            .await
            .context("pump.swap buy quote simulation rpc call failed")?;
        let logs = sim.value.logs.unwrap_or_default();
        if let Some(err) = sim.value.err {
            warn!(
                "pump.swap buy quote simulation failed for pool {} buyer {}: {:?}; logs: {}",
                pool,
                buyer,
                err,
                logs.join(" | ")
            );
            anyhow::bail!("pump.swap buy quote simulation failed: {err:?}");
        }

        let quoted = Self::parse_logs(logs.iter(), None)
            .into_iter()
            .rev()
            .find_map(|event| match event {
                PumpSwapEvent::Buy(Some(buy))
                    if buy.pool.to_bytes() == pool.to_bytes()
                        && buy.base_amount_out > 0
                        && buy.ix_name == expected_ix_name =>
                {
                    Some(buy.base_amount_out)
                }
                _ => None,
            });
        if quoted.is_none() {
            warn!(
                "pump.swap buy quote simulation missing {} event for pool {} buyer {}; logs: {}",
                expected_ix_name,
                pool,
                buyer,
                logs.join(" | ")
            );
        }
        quoted.with_context(|| {
            format!(
                "pump.swap buy quote simulation missing {} event",
                expected_ix_name
            )
        })
    }

    async fn simulate_sell_output_raw(
        &self,
        buyer: &Pubkey,
        instructions: &[Instruction],
        pool: &Pubkey,
        expected_base_amount_in: u64,
    ) -> anyhow::Result<u64> {
        let (blockhash, _) = self
            .sol
            .get_latest_blockhash_with_commitment_resilient(CommitmentConfig::processed())
            .await
            .context("failed to fetch blockhash for pump.swap sell quote simulation")?;
        let tx = compile_unsigned_v0_transaction(buyer, instructions, blockhash)
            .context("failed to compile unsigned tx for pump.swap sell quote simulation")?;
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
            .await
            .context("pump.swap sell quote simulation rpc call failed")?;
        let logs = sim.value.logs.unwrap_or_default();
        if let Some(err) = sim.value.err {
            warn!(
                "pump.swap sell quote simulation failed for pool {} buyer {}: {:?}; logs: {}",
                pool,
                buyer,
                err,
                logs.join(" | ")
            );
            anyhow::bail!("pump.swap sell quote simulation failed: {err:?}");
        }

        let quoted = Self::parse_logs(logs.iter(), None)
            .into_iter()
            .rev()
            .find_map(|event| match event {
                PumpSwapEvent::Sell(Some(sell))
                    if sell.base_amount_in == expected_base_amount_in
                        && sell.user_quote_amount_out > 0 =>
                {
                    Some(sell.user_quote_amount_out)
                }
                _ => None,
            });
        if quoted.is_none() {
            warn!(
                "pump.swap sell quote simulation missing sell event for pool {} buyer {}; logs: {}",
                pool,
                buyer,
                logs.join(" | ")
            );
        }
        quoted.context("pump.swap sell quote simulation missing sell event")
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

    fn protocol_fee_recipient_for_mode(is_mayhem_mode: bool) -> Pubkey {
        if is_mayhem_mode {
            MAYHEM_FEE_RECIPIENT
        } else {
            PROTOCOL_FEE_RECIP
        }
    }

    fn protocol_fee_accounts_for_mode(
        &self,
        is_mayhem_mode: bool,
        quote_program: &Pubkey,
    ) -> anyhow::Result<(Pubkey, Pubkey)> {
        let recipient = Self::protocol_fee_recipient_for_mode(is_mayhem_mode);
        if *quote_program == TOKEN_PROGRAM_ID {
            Ok((
                recipient,
                self.sol.get_ata_for_token(&recipient, &WSOL_MINT),
            ))
        } else if *quote_program == TOKEN_2022_PROGRAM_ID {
            Ok((
                recipient,
                self.sol.get_ata_for_token2022(&recipient, &WSOL_MINT),
            ))
        } else {
            anyhow::bail!(
                "unsupported quote token program for protocol fee recipient ATA: {}",
                quote_program
            );
        }
    }

    pub async fn fetch_global_config(&self) -> anyhow::Result<GlobalConfig> {
        let state = self
            .sol
            .get_account_with_commitment_resilient(
                &GLOBAL_CONFIG_PUB,
                CommitmentConfig::processed(),
            )
            .await
            .context("global config account not found")?
            .data;
        let mut cursor = Cursor::new(&state[8..]);
        Ok(GlobalConfig::deserialize_reader(&mut cursor)?)
    }

    async fn protocol_fee_accounts_for_pool(
        &self,
        state: &Pool,
        quote_program: &Pubkey,
    ) -> anyhow::Result<(Pubkey, Pubkey)> {
        let global_config = match self.fetch_global_config().await {
            Ok(config) => config,
            Err(error) => {
                if state.is_mayhem_mode {
                    warn!(
                        "failed to fetch pump.swap global config for mayhem fee recipient: {error}"
                    );
                } else {
                    warn!(
                        "failed to fetch pump.swap global config for protocol fee recipient: {error}"
                    );
                }
                return self.protocol_fee_accounts_for_mode(state.is_mayhem_mode, quote_program);
            }
        };

        let recipient = if state.is_mayhem_mode {
            Pubkey::new_from_array(global_config.reserved_fee_recipient.to_bytes())
        } else {
            let idx = (state.index as usize) % global_config.protocol_fee_recipients.len();
            Pubkey::new_from_array(global_config.protocol_fee_recipients[idx].to_bytes())
        };
        match quote_program {
            program if *program == TOKEN_PROGRAM_ID => Ok((
                recipient,
                self.sol.get_ata_for_token(&recipient, &WSOL_MINT),
            )),
            program if *program == TOKEN_2022_PROGRAM_ID => Ok((
                recipient,
                self.sol.get_ata_for_token2022(&recipient, &WSOL_MINT),
            )),
            _ => anyhow::bail!(
                "unsupported quote token program for protocol fee recipient ATA: {}",
                quote_program
            ),
        }
    }

    pub async fn derive_creator_vault(&self, creator: &Pubkey) -> anyhow::Result<(Pubkey, Pubkey)> {
        let (pda, _) =
            Pubkey::find_program_address(&[b"creator_vault", creator.as_ref()], &PUMP_SWAP_ID);
        let vault_ata = self.sol.get_ata_for_token(&pda, &WSOL_MINT);
        Ok((pda, vault_ata))
    }

    pub async fn derive_uv_accu(&self, user: &Pubkey) -> anyhow::Result<Pubkey> {
        let (pda, _) = Pubkey::find_program_address(
            &[b"user_volume_accumulator", user.as_ref()],
            &PUMP_SWAP_ID,
        );
        Ok(pda)
    }

    fn pool_v2_pda(base_mint: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[b"pool-v2", base_mint.as_ref()], &PUMP_SWAP_ID).0
    }

    async fn fetch_account_data_raw(&self, pubkey: &Pubkey) -> anyhow::Result<Vec<u8>> {
        Ok(self
            .sol
            .get_account_with_commitment_resilient(pubkey, CommitmentConfig::processed())
            .await?
            .data)
    }

    fn decode_buyback_fee_recipients_from_global_config(
        account_data: &[u8],
    ) -> anyhow::Result<Vec<Pubkey>> {
        anyhow::ensure!(
            account_data.len() >= 8,
            "pump.swap global config account data is too small"
        );
        let mut cursor = Cursor::new(&account_data[8..]);
        let _legacy = GlobalConfig::deserialize_reader(&mut cursor)
            .context("failed to decode legacy pump.swap global config layout")?;
        let extras_offset = 8 + cursor.position() as usize;
        if account_data.len() < extras_offset + 1 + 32 * BUYBACK_FEE_RECIPIENT_COUNT + 8 {
            return Ok(Vec::new());
        }

        let mut offset = extras_offset + 1; // skip is_cashback_enabled
        let mut recipients = Vec::with_capacity(BUYBACK_FEE_RECIPIENT_COUNT);
        for _ in 0..BUYBACK_FEE_RECIPIENT_COUNT {
            let bytes: [u8; 32] = account_data[offset..offset + 32]
                .try_into()
                .context("failed to decode pump.swap buyback fee recipient pubkey")?;
            let recipient = Pubkey::new_from_array(bytes);
            if recipient != Pubkey::default() {
                recipients.push(recipient);
            }
            offset += 32;
        }
        Ok(recipients)
    }

    fn decode_is_cashback_coin_from_pool(account_data: &[u8]) -> anyhow::Result<bool> {
        anyhow::ensure!(
            account_data.len() >= 8,
            "pump.swap pool account data is too small"
        );
        let mut cursor = Cursor::new(&account_data[8..]);
        let _legacy =
            Pool::deserialize_reader(&mut cursor).context("failed to decode pump.swap pool")?;
        let extras_offset = 8 + cursor.position() as usize;
        if account_data.len() <= extras_offset {
            return Ok(false);
        }
        match account_data[extras_offset] {
            0 => Ok(false),
            1 => Ok(true),
            value => anyhow::bail!("invalid pump.swap cashback flag value: {value}"),
        }
    }

    async fn buy_remaining_accounts_for_pool(
        &self,
        state: &Pool,
        pool: &Pubkey,
        user_volume_accu: &Pubkey,
        quote_program: &Pubkey,
    ) -> anyhow::Result<Vec<AccountMeta>> {
        let mut remaining = Vec::new();

        let pool_data = self
            .fetch_account_data_raw(pool)
            .await
            .context("failed to fetch raw pump.swap pool account for remaining accounts")?;
        if Self::decode_is_cashback_coin_from_pool(&pool_data)? {
            let user_volume_accu_wsol_ata = match quote_program {
                program if *program == TOKEN_PROGRAM_ID => {
                    self.sol.get_ata_for_token(user_volume_accu, &WSOL_MINT)
                }
                program if *program == TOKEN_2022_PROGRAM_ID => {
                    self.sol.get_ata_for_token2022(user_volume_accu, &WSOL_MINT)
                }
                _ => anyhow::bail!(
                    "unsupported quote token program for pump.swap cashback ATA: {}",
                    quote_program
                ),
            };
            remaining.push(AccountMeta::new(user_volume_accu_wsol_ata, false));
        }

        let coin_creator = Pubkey::new_from_array(state.coin_creator.to_bytes());
        if coin_creator != Pubkey::default() {
            let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
            remaining.push(AccountMeta::new_readonly(
                Self::pool_v2_pda(&base_mint),
                false,
            ));
        }

        let global_config_data = self
            .fetch_account_data_raw(&GLOBAL_CONFIG_PUB)
            .await
            .context("failed to fetch raw pump.swap global config for buyback recipients")?;
        let buyback_recipients =
            Self::decode_buyback_fee_recipients_from_global_config(&global_config_data)?;
        if let Some(buyback_fee_recipient) = buyback_recipients.first().copied() {
            let buyback_fee_recipient_ata = match quote_program {
                program if *program == TOKEN_PROGRAM_ID => self
                    .sol
                    .get_ata_for_token(&buyback_fee_recipient, &WSOL_MINT),
                program if *program == TOKEN_2022_PROGRAM_ID => self
                    .sol
                    .get_ata_for_token2022(&buyback_fee_recipient, &WSOL_MINT),
                _ => anyhow::bail!(
                    "unsupported quote token program for pump.swap buyback ATA: {}",
                    quote_program
                ),
            };
            remaining.push(AccountMeta::new_readonly(buyback_fee_recipient, false));
            remaining.push(AccountMeta::new(buyback_fee_recipient_ata, false));
        }

        Ok(remaining)
    }

    async fn sell_remaining_accounts_for_pool(
        &self,
        state: &Pool,
        pool: &Pubkey,
        user_volume_accu: &Pubkey,
        quote_program: &Pubkey,
    ) -> anyhow::Result<Vec<AccountMeta>> {
        let mut remaining = Vec::new();

        let pool_data = self
            .fetch_account_data_raw(pool)
            .await
            .context("failed to fetch raw pump.swap pool account for sell remaining accounts")?;
        if Self::decode_is_cashback_coin_from_pool(&pool_data)? {
            let user_volume_accu_wsol_ata = match quote_program {
                program if *program == TOKEN_PROGRAM_ID => {
                    self.sol.get_ata_for_token(user_volume_accu, &WSOL_MINT)
                }
                program if *program == TOKEN_2022_PROGRAM_ID => {
                    self.sol.get_ata_for_token2022(user_volume_accu, &WSOL_MINT)
                }
                _ => anyhow::bail!(
                    "unsupported quote token program for pump.swap cashback ATA: {}",
                    quote_program
                ),
            };
            remaining.push(AccountMeta::new(user_volume_accu_wsol_ata, false));
            remaining.push(AccountMeta::new(*user_volume_accu, false));
        }

        let coin_creator = Pubkey::new_from_array(state.coin_creator.to_bytes());
        if coin_creator != Pubkey::default() {
            let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
            remaining.push(AccountMeta::new_readonly(
                Self::pool_v2_pda(&base_mint),
                false,
            ));
        }

        let global_config_data = self
            .fetch_account_data_raw(&GLOBAL_CONFIG_PUB)
            .await
            .context("failed to fetch raw pump.swap global config for sell buyback recipients")?;
        let buyback_recipients =
            Self::decode_buyback_fee_recipients_from_global_config(&global_config_data)?;
        if let Some(buyback_fee_recipient) = buyback_recipients.first().copied() {
            let buyback_fee_recipient_ata = match quote_program {
                program if *program == TOKEN_PROGRAM_ID => self
                    .sol
                    .get_ata_for_token(&buyback_fee_recipient, &WSOL_MINT),
                program if *program == TOKEN_2022_PROGRAM_ID => self
                    .sol
                    .get_ata_for_token2022(&buyback_fee_recipient, &WSOL_MINT),
                _ => anyhow::bail!(
                    "unsupported quote token program for pump.swap buyback ATA: {}",
                    quote_program
                ),
            };
            remaining.push(AccountMeta::new_readonly(buyback_fee_recipient, false));
            remaining.push(AccountMeta::new(buyback_fee_recipient_ata, false));
        }

        Ok(remaining)
    }

    pub async fn fetch_state(&self, pool: &Pubkey) -> Result<Pool, anyhow::Error> {
        if let Some(cached_state) = self.pool_state_cache.lock().await.get(pool).cloned() {
            return Self::decode_pool_state_bytes(pool, &cached_state);
        }

        let state = self
            .sol
            .get_account_with_commitment_resilient(pool, CommitmentConfig::processed())
            .await?
            .data;
        self.cache_pool_state_bytes(pool, &state).await;
        Self::decode_pool_state_bytes(pool, &state)
    }

    /// find PumpSwap pools by base mint (and optional quote mint) via on-chain memcmp filters
    /// returns pool addresses that match the given mint(s)
    async fn find_pools_by_lookup_spec(&self, spec: PoolLookupSpec) -> anyhow::Result<Vec<Pubkey>> {
        use anyhow::Context as _;
        use base64::Engine;
        use reqwest::Client;
        use serde::Deserialize;
        use serde_json::json;

        // PumpSwap mainnet scans on Helius currently fail when we combine the field memcmp filters
        // with a discriminator memcmp. Query by the stable field offsets and let fetch_state()
        // validate the returned account shape.
        #[derive(Debug, Deserialize)]
        struct RpcErrorBody {
            code: i64,
            message: String,
        }

        #[derive(Debug, Deserialize)]
        struct ProgramAccountsAccount {
            data: (String, String),
        }

        #[derive(Debug, Deserialize)]
        struct ProgramAccountsEntry {
            pubkey: String,
            account: ProgramAccountsAccount,
        }

        #[derive(Debug, Deserialize)]
        struct ProgramAccountsResponse {
            result: Option<Vec<ProgramAccountsEntry>>,
            error: Option<RpcErrorBody>,
        }

        let mut filters = Vec::new();

        if let Some(base_mint) = spec.base_mint {
            filters.push(json!({
                "memcmp": {
                    "offset": BASE_MINT_OFFSET,
                    "bytes": base_mint.to_string(),
                }
            }));
        }

        if let Some(q) = spec.quote_mint {
            filters.push(json!({
                "memcmp": {
                    "offset": QUOTE_MINT_OFFSET,
                    "bytes": q.to_string(),
                }
            }));
        }

        let client = Client::new();
        let response = client
            .post(self.sol.rpc_client.url())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": "pump-swap-pool-discovery",
                "method": "getProgramAccounts",
                "params": [
                    PUMP_SWAP_ID.to_string(),
                    {
                        "commitment": "confirmed",
                        "encoding": "base64",
                        "filters": filters,
                    }
                ]
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("pump.swap getProgramAccounts read body failed")?;
        anyhow::ensure!(
            status.is_success(),
            "pump.swap getProgramAccounts http {status}: {body}"
        );
        let parsed: ProgramAccountsResponse = serde_json::from_str(&body)
            .context("pump.swap getProgramAccounts response decode failed")?;
        if let Some(error) = parsed.error {
            anyhow::bail!(
                "pump.swap getProgramAccounts rpc error {}: {}",
                error.code,
                error.message
            );
        }

        let mut pools = Vec::new();
        for entry in parsed.result.unwrap_or_default() {
            let pool = entry
                .pubkey
                .parse::<Pubkey>()
                .with_context(|| format!("invalid pump.swap pool pubkey {}", entry.pubkey))?;
            let state = base64::prelude::BASE64_STANDARD
                .decode(entry.account.data.0.as_bytes())
                .with_context(|| format!("failed to decode base64 pump.swap pool {}", pool))?;
            if let Err(error) = Self::decode_pool_state_bytes(&pool, &state) {
                warn!(
                    "ignoring malformed pump.swap pool candidate {} discovered via mint filters: {}",
                    pool, error
                );
                continue;
            }
            self.cache_pool_state_bytes(&pool, &state).await;
            pools.push(pool);
        }

        Ok(pools)
    }

    pub async fn find_pools_by_mint(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        let mut pools = Vec::new();
        for spec in Self::pool_lookup_specs(mint) {
            for pool in self.find_pools_by_lookup_spec(spec).await? {
                if !pools.contains(&pool) {
                    pools.push(pool);
                }
            }
        }

        if quote_mint.is_none() {
            return Ok(pools);
        }

        let mut filtered = Vec::new();
        for pool in pools {
            let state = match self.fetch_state(&pool).await {
                Ok(state) => state,
                Err(error) => {
                    warn!(
                        "pump.swap quoted mint lookup could not decode pool {} for mint {}: {}",
                        pool, mint, error
                    );
                    continue;
                }
            };
            if Self::liquidity_token_account_for_mint_pair(&state, mint, quote_mint).is_some() {
                filtered.push(pool);
            }
        }

        Ok(filtered)
    }

    pub async fn find_pools_by_creator(&self, creator: &Pubkey) -> anyhow::Result<Vec<Pubkey>> {
        use solana_account_decoder_client_types::{UiAccountEncoding, UiDataSliceConfig};
        use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
        use solana_client::rpc_filter::{Memcmp, RpcFilterType};

        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                CREATOR_OFFSET,
                creator.as_ref(),
            ))]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                data_slice: Some(UiDataSliceConfig {
                    offset: 0,
                    length: 0,
                }),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };

        let acct_list = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&PUMP_SWAP_ID, cfg)
            .await?;

        Ok(acct_list.into_iter().map(|(k, _)| k).collect())
    }

    pub async fn find_pools_by_creator_resilient(
        &self,
        creator: &Pubkey,
        rpc_url: Option<&str>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        match self.find_pools_by_creator(creator).await {
            Ok(pools) => Ok(pools),
            Err(error) => {
                let rpc_url = rpc_url.map(str::trim).filter(|value| !value.is_empty());
                if !Self::should_retry_paginated_creator_lookup(&error)
                    || !rpc_url.is_some_and(Self::is_helius_rpc_url)
                {
                    return Err(error);
                }

                let initial_error = error.to_string();
                warn!(
                    "pump.swap creator discovery hit RPC account limit; retrying with Helius getProgramAccountsV2 pagination"
                );
                self.find_pools_by_creator_paginated_helius(
                    creator,
                    rpc_url.expect("checked is_some above"),
                )
                .await
                .with_context(|| {
                    format!(
                        "helius getProgramAccountsV2 fallback failed after standard creator lookup error: {initial_error}"
                    )
                })
            }
        }
    }

    pub async fn find_pools_by_lp_mint(&self, lp_mint: &Pubkey) -> anyhow::Result<Vec<Pubkey>> {
        use solana_account_decoder_client_types::{UiAccountEncoding, UiDataSliceConfig};
        use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
        use solana_client::rpc_filter::{Memcmp, RpcFilterType};

        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                LP_MINT_OFFSET,
                lp_mint.as_ref(),
            ))]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                data_slice: Some(UiDataSliceConfig {
                    offset: 0,
                    length: 0,
                }),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };

        let acct_list = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&PUMP_SWAP_ID, cfg)
            .await?;

        Ok(acct_list.into_iter().map(|(k, _)| k).collect())
    }

    fn is_helius_rpc_url(rpc_url: &str) -> bool {
        rpc_url
            .split('?')
            .next()
            .is_some_and(|base| base.contains("helius-rpc.com"))
    }

    fn should_retry_paginated_creator_lookup(error: &anyhow::Error) -> bool {
        let message = error.to_string().to_ascii_lowercase();
        message.contains("too many accounts requested")
            || message.contains("getprogramaccountsv2")
            || message.contains("pagination")
    }

    async fn find_pools_by_creator_paginated_helius(
        &self,
        creator: &Pubkey,
        rpc_url: &str,
    ) -> anyhow::Result<Vec<Pubkey>> {
        use anyhow::Context as _;
        use reqwest::Client;
        use serde::Deserialize;
        use serde_json::json;
        use std::collections::BTreeSet;

        const HELIUS_PROGRAM_ACCOUNTS_PAGE_LIMIT: u64 = 1_000;

        #[derive(Debug, Deserialize)]
        struct ProgramAccountsV2Response {
            result: Option<ProgramAccountsV2Result>,
            error: Option<ProgramAccountsV2Error>,
        }

        #[derive(Debug, Deserialize)]
        struct ProgramAccountsV2Result {
            accounts: Vec<ProgramAccountsV2Account>,
            #[serde(rename = "paginationKey")]
            pagination_key: Option<String>,
        }

        #[derive(Debug, Deserialize)]
        struct ProgramAccountsV2Account {
            pubkey: String,
        }

        #[derive(Debug, Deserialize)]
        struct ProgramAccountsV2Error {
            code: i64,
            message: String,
        }

        let client = Client::new();
        let creator_bytes = creator.to_string();
        let mut pagination_key = None::<String>;
        let mut pools = BTreeSet::new();

        loop {
            let mut options = json!({
                "commitment": "confirmed",
                "encoding": "base64",
                "dataSlice": {
                    "offset": 0,
                    "length": 0
                },
                "filters": [
                    {
                        "memcmp": {
                            "offset": CREATOR_OFFSET,
                            "bytes": creator_bytes
                        }
                    }
                ],
                "limit": HELIUS_PROGRAM_ACCOUNTS_PAGE_LIMIT
            });
            if let Some(cursor) = pagination_key.as_ref() {
                options["paginationKey"] = json!(cursor);
            }

            let response = client
                .post(rpc_url)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": "pump-swap-creator-discovery",
                    "method": "getProgramAccountsV2",
                    "params": [PUMP_SWAP_ID.to_string(), options]
                }))
                .send()
                .await
                .context("helius getProgramAccountsV2 http request failed")?;
            let status = response.status();
            let body = response
                .text()
                .await
                .context("helius getProgramAccountsV2 read body failed")?;
            anyhow::ensure!(
                status.is_success(),
                "helius getProgramAccountsV2 http {status}: {body}"
            );

            let parsed: ProgramAccountsV2Response = serde_json::from_str(&body)
                .context("helius getProgramAccountsV2 response decode failed")?;
            if let Some(error) = parsed.error {
                anyhow::bail!(
                    "helius getProgramAccountsV2 rpc error {}: {}",
                    error.code,
                    error.message
                );
            }
            let result = parsed
                .result
                .context("helius getProgramAccountsV2 missing result")?;

            for account in result.accounts.iter() {
                pools.insert(
                    account
                        .pubkey
                        .parse::<Pubkey>()
                        .with_context(|| format!("invalid pool pubkey {}", account.pubkey))?,
                );
            }

            if result.accounts.is_empty() || result.pagination_key.is_none() {
                break;
            }
            pagination_key = result.pagination_key;
        }

        Ok(pools.into_iter().collect())
    }

    pub async fn estimate_withdraw_amounts_raw(
        &self,
        pool: &Pubkey,
        lp_token_amount: u64,
    ) -> anyhow::Result<(Pool, u64, u64)> {
        anyhow::ensure!(
            lp_token_amount > 0,
            "pump.swap withdraw lp amount must be > 0"
        );

        let state = self.fetch_state(pool).await?;
        anyhow::ensure!(state.lp_supply > 0, "pump.swap pool lp_supply is zero");
        anyhow::ensure!(
            lp_token_amount <= state.lp_supply,
            "pump.swap withdraw lp amount exceeds pool lp_supply"
        );

        let pool_base_token_account =
            Pubkey::new_from_array(state.pool_base_token_account.to_bytes());
        let pool_quote_token_account =
            Pubkey::new_from_array(state.pool_quote_token_account.to_bytes());
        let base_vault_raw = self.token_balance_raw(&pool_base_token_account).await?;
        let quote_vault_raw = self.token_balance_raw(&pool_quote_token_account).await?;

        let base_out = ((u128::from(base_vault_raw) * u128::from(lp_token_amount))
            / u128::from(state.lp_supply)) as u64;
        let quote_out = ((u128::from(quote_vault_raw) * u128::from(lp_token_amount))
            / u128::from(state.lp_supply)) as u64;
        anyhow::ensure!(
            base_out > 0 || quote_out > 0,
            "pump.swap withdraw quote resulted in zero outputs"
        );

        Ok((state, base_out, quote_out))
    }

    pub async fn withdraw_for_user(
        &self,
        owner: &Pubkey,
        pool: &Pubkey,
        lp_token_amount: u64,
        min_base_amount_out: u64,
        min_quote_amount_out: u64,
    ) -> anyhow::Result<(Vec<Instruction>, Pool, u64, u64)> {
        let owner = *owner;
        let (state, base_out, quote_out) = self
            .estimate_withdraw_amounts_raw(pool, lp_token_amount)
            .await?;

        let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
        let quote_mint = Pubkey::new_from_array(state.quote_mint.to_bytes());
        let lp_mint = Pubkey::new_from_array(state.lp_mint.to_bytes());
        let pool_base_token_account =
            Pubkey::new_from_array(state.pool_base_token_account.to_bytes());
        let pool_quote_token_account =
            Pubkey::new_from_array(state.pool_quote_token_account.to_bytes());

        let base_program = self
            .sol
            .get_token_program_id(&base_mint)
            .await
            .with_context(|| format!("failed to resolve token program for {}", base_mint))?;
        let quote_program = self
            .sol
            .get_token_program_id(&quote_mint)
            .await
            .with_context(|| format!("failed to resolve token program for {}", quote_mint))?;

        let user_base_token_account = if base_program == TOKEN_PROGRAM_ID {
            self.sol.get_ata_for_token(&owner, &base_mint)
        } else {
            self.sol.get_ata_for_token2022(&owner, &base_mint)
        };
        let user_quote_token_account = if quote_program == TOKEN_PROGRAM_ID {
            self.sol.get_ata_for_token(&owner, &quote_mint)
        } else {
            self.sol.get_ata_for_token2022(&owner, &quote_mint)
        };
        let user_pool_token_account = self.sol.get_ata_for_token2022(&owner, &lp_mint);
        let event_authority =
            Pubkey::find_program_address(&[b"__event_authority"], &PUMP_SWAP_ID).0;

        let mut instructions = Vec::new();
        instructions.push(create_associated_token_account_idempotent(
            &owner,
            &owner,
            &base_mint,
            &base_program,
        ));
        instructions.push(create_associated_token_account_idempotent(
            &owner,
            &owner,
            &quote_mint,
            &quote_program,
        ));

        let accounts = pump_swap_types::accounts::Withdraw {
            pool: pool.to_bytes().into(),
            global_config: GLOBAL_CONFIG_PUB.to_bytes().into(),
            user: owner.to_bytes().into(),
            base_mint: base_mint.to_bytes().into(),
            quote_mint: quote_mint.to_bytes().into(),
            lp_mint: lp_mint.to_bytes().into(),
            user_base_token_account: user_base_token_account.to_bytes().into(),
            user_quote_token_account: user_quote_token_account.to_bytes().into(),
            user_pool_token_account: user_pool_token_account.to_bytes().into(),
            pool_base_token_account: pool_base_token_account.to_bytes().into(),
            pool_quote_token_account: pool_quote_token_account.to_bytes().into(),
            token_program: TOKEN_PROGRAM_ID.to_bytes().into(),
            token_2022_program: TOKEN_2022_PROGRAM_ID.to_bytes().into(),
            event_authority: event_authority.to_bytes().into(),
            program: PUMP_SWAP_ID.to_bytes().into(),
        };
        let ix_data = pump_swap_types::instruction::Withdraw {
            _lp_token_amount_in: lp_token_amount,
            _min_base_amount_out: min_base_amount_out,
            _min_quote_amount_out: min_quote_amount_out,
        };
        instructions.push(Instruction {
            program_id: PUMP_SWAP_ID,
            accounts: accounts
                .to_account_metas(None)
                .into_iter()
                .map(|meta| AccountMeta {
                    pubkey: Pubkey::new_from_array(meta.pubkey.to_bytes()),
                    is_signer: meta.is_signer,
                    is_writable: meta.is_writable,
                })
                .collect(),
            data: ix_data.data(),
        });

        Ok((instructions, state, base_out, quote_out))
    }

    pub async fn fetch_wsol_liquidity_raw(&self, state: &Pool) -> anyhow::Result<u64> {
        let base_mint = Pubkey::new_from_array(state.base_mint.to_bytes());
        let quote_mint = Pubkey::new_from_array(state.quote_mint.to_bytes());
        if quote_mint == WSOL_MINT {
            let quote_ata = Address::from(state.pool_quote_token_account.to_bytes());
            let quote_balance = self.sol.get_token_balance_from_ata(&quote_ata).await?;
            return Ok((quote_balance * 1e9).max(0.0) as u64);
        }
        if base_mint == WSOL_MINT {
            let base_ata = Address::from(state.pool_base_token_account.to_bytes());
            let base_balance = self.sol.get_token_balance_from_ata(&base_ata).await?;
            return Ok((base_balance * 1e9).max(0.0) as u64);
        }
        anyhow::bail!("pump.swap pool is not WSOL quoted")
    }

    async fn fetch_liquidity_raw_for_mint_pair(
        &self,
        state: &Pool,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<u64> {
        let desired_quote_mint = quote_mint.copied().unwrap_or(WSOL_MINT);
        let liquidity_token_account = Self::liquidity_token_account_for_mint_pair(
            state, mint, quote_mint,
        )
        .with_context(|| {
            format!(
                "pump.swap pool does not match desired mint pair {} / {}",
                mint, desired_quote_mint
            )
        })?;
        let (raw_balance, _) = self
            .sol
            .get_token_balance_raw_from_ata(&liquidity_token_account)
            .await
            .with_context(|| {
                format!(
                    "failed to fetch pump.swap quote liquidity for mint pair {} / {} from {}",
                    mint, desired_quote_mint, liquidity_token_account
                )
            })?;
        Ok(raw_balance)
    }

    pub async fn find_pool_by_mint_with_min_liquidity(
        &self,
        base_mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
        min_liquidity_raw: u64,
    ) -> anyhow::Result<Option<Pubkey>> {
        let pools = self.find_pools_by_mint(base_mint, quote_mint).await?;
        let mut matching_pools = Vec::new();
        let mut liquidity_probe_failed = false;

        for pool in pools {
            let state = match self.fetch_state(&pool).await {
                Ok(state) => state,
                Err(error) => {
                    liquidity_probe_failed = true;
                    warn!(
                        "pump.swap mint lookup could not decode pool {} for mint {}: {}",
                        pool, base_mint, error
                    );
                    continue;
                }
            };
            if Self::liquidity_token_account_for_mint_pair(&state, base_mint, quote_mint).is_none()
            {
                continue;
            }
            matching_pools.push((pool, state));
        }

        if min_liquidity_raw <= 1 && matching_pools.len() == 1 {
            return Ok(matching_pools.first().map(|(pool, _)| *pool));
        }

        let mut best_pool = None;
        let mut best_liquidity = 0u64;
        let first_matching_pool = matching_pools.first().map(|(pool, _)| *pool);

        for (pool, state) in matching_pools {
            let liquidity = match self
                .fetch_liquidity_raw_for_mint_pair(&state, base_mint, quote_mint)
                .await
            {
                Ok(liquidity) => liquidity,
                Err(error) => {
                    liquidity_probe_failed = true;
                    warn!(
                        "pump.swap mint lookup could not fetch desired quote liquidity for pool {} mint {}: {}",
                        pool, base_mint, error
                    );
                    continue;
                }
            };
            if liquidity >= min_liquidity_raw && liquidity >= best_liquidity {
                best_liquidity = liquidity;
                best_pool = Some(pool);
            }
        }

        if best_pool.is_none()
            && liquidity_probe_failed
            && min_liquidity_raw <= 1
            && let Some(pool) = first_matching_pool
        {
            warn!(
                "pump.swap mint lookup is using first matching mint-pair pool {} for mint {} after liquidity probes failed",
                pool, base_mint
            );
            return Ok(Some(pool));
        }

        Ok(best_pool)
    }

    pub async fn fetch_price(&self, pool: &Pubkey) -> anyhow::Result<(Pool, f64)> {
        let state = self.fetch_state(pool).await?;
        let vsr = self
            .sol
            .get_token_balance_from_ata(&Address::from(state.pool_quote_token_account.to_bytes()))
            .await?;
        let vtr = self
            .sol
            .get_token_balance_from_ata(&Address::from(state.pool_base_token_account.to_bytes()))
            .await?;
        let price = vsr as f64 / vtr as f64;
        Ok((state, price))
    }

    pub async fn get_mint_from_pool(&self, pool: &Pubkey) -> anyhow::Result<Pubkey> {
        let pool = self.fetch_state(pool).await?;
        let base_mint = Address::from(pool.base_mint.to_bytes());
        let quote_mint = Address::from(pool.quote_mint.to_bytes());

        // PumpSwap pools can be oriented either way; for mint-level aggregation we always want the
        // non-WSOL mint when the pool is WSOL-quoted, otherwise we end up tracking WSOL itself as a
        // "mint" (noise) for base=WSOL pools.
        if base_mint == WSOL_MINT && quote_mint != WSOL_MINT {
            return Ok(quote_mint);
        }
        if quote_mint == WSOL_MINT && base_mint != WSOL_MINT {
            return Ok(base_mint);
        }

        Ok(base_mint)
    }

    pub fn parse_logs(
        logs: std::slice::Iter<'_, String>,
        sig: Option<&String>,
    ) -> Vec<PumpSwapEvent> {
        let mut events: Vec<PumpSwapEvent> = Vec::new();
        for log in logs {
            let is_program_data = log.contains(SEARCH_FOR);
            if is_program_data {
                let program_data = log.split_at(SEARCH_FOR.len());
                let (_, program_data) = program_data;
                let b64 = match decode_b64(program_data) {
                    Ok(b64) => b64,
                    Err(_) => {
                        continue;
                    }
                };
                if b64.len() < 8 {
                    continue;
                }
                if b64[..8] == CREATE_POOL_EVENT_DISCRIM {
                    let mut cur = Cursor::new(&b64[8..]);
                    let dtx = CreatePoolEvent::deserialize_reader(&mut cur);
                    match dtx {
                        Ok(dtx) => {
                            events.push(PumpSwapEvent::CreatePool(Some(dtx)));
                        }
                        Err(e) => {
                            warn!(
                                "Error deserializing create pool event {:?}: {e}",
                                sig.unwrap_or(&"".to_string())
                            );
                        }
                    }
                } else if b64[..8] == BUY_EVENT_DISCRIM {
                    let mut cur = Cursor::new(&b64[8..]);
                    let dtx = BuyEvent::deserialize_reader(&mut cur);
                    match dtx {
                        Ok(dtx) => {
                            events.push(PumpSwapEvent::Buy(Some(dtx)));
                        }
                        Err(e) => {
                            warn!(
                                "Error deserializing buy event {:?}: {e}",
                                sig.unwrap_or(&"".to_string())
                            );
                        }
                    }
                } else if b64[..8] == SELL_EVENT_DISCRIM {
                    let mut cur = Cursor::new(&b64[8..]);
                    let dtx = SellEvent::deserialize_reader(&mut cur);
                    match dtx {
                        Ok(dtx) => {
                            events.push(PumpSwapEvent::Sell(Some(dtx)));
                        }
                        Err(e) => {
                            warn!(
                                "Error deserializing sell event {:?}: {e}",
                                sig.unwrap_or(&"".to_string())
                            );
                        }
                    }
                } else if b64[..8] == DEPOSIT_EVENT_DISCRIM {
                    let mut cur = Cursor::new(&b64[8..]);
                    let dtx = DepositEvent::deserialize_reader(&mut cur);
                    match dtx {
                        Ok(dtx) => {
                            events.push(PumpSwapEvent::Deposit(Some(dtx)));
                        }
                        // Deposit event fields can drift upstream; they're not used by our
                        // mint-flow pipeline, so treat decode misses as non-critical noise.
                        Err(_) => events.push(PumpSwapEvent::Unknown),
                    }
                } else if b64[..8] == WITHDRAW_EVENT_DISCRIM {
                    let mut cur = Cursor::new(&b64[8..]);
                    let dtx = WithdrawEvent::deserialize_reader(&mut cur);
                    match dtx {
                        Ok(dtx) => {
                            events.push(PumpSwapEvent::Withdraw(Some(dtx)));
                        }
                        // Withdraw event fields can drift upstream; they're not used by our
                        // mint-flow pipeline, so treat decode misses as non-critical noise.
                        Err(_) => events.push(PumpSwapEvent::Unknown),
                    }
                } else {
                    events.push(PumpSwapEvent::Unknown);
                }
            }
        }
        events
    }

    pub fn price_from_create(create: &CreatePoolEvent) -> f64 {
        let base_raw = if create.pool_base_amount > 0 {
            create.pool_base_amount
        } else {
            create.base_amount_in
        };
        let quote_raw = if create.pool_quote_amount > 0 {
            create.pool_quote_amount
        } else {
            create.quote_amount_in
        };
        if base_raw == 0 {
            return 0.0;
        }

        let vtr = base_raw as f64 / 1e6; // token
        let vsr = quote_raw as f64 / 1e9; // sol
        vsr / vtr
    }

    pub fn price_from_buy(buy: &BuyEvent) -> f64 {
        let vtr = buy.pool_base_token_reserves as f64 / 1e6;
        let vsr = buy.pool_quote_token_reserves as f64 / 1e9;
        vsr / vtr
    }

    pub fn price_from_sell(sell: &SellEvent) -> f64 {
        let vtr = sell.pool_base_token_reserves as f64 / 1e6;
        let vsr = sell.pool_quote_token_reserves as f64 / 1e9;
        vsr / vtr
    }

    pub fn price_from_reserves(vtr: u64, vsr: u64) -> f64 {
        let vtr = vtr as f64 / 1e6;
        let vsr = vsr as f64 / 1e9;
        vsr / vtr
    }

    pub async fn get_market_cap(vtr: u64, vsr: u64, sol_price_usd: f64) -> anyhow::Result<f64> {
        let price = Self::price_from_reserves(vtr, vsr);
        let price_in_usd = price * sol_price_usd;
        Ok(1000000000.0 * price_in_usd)
    }

    pub async fn buy(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.buy_with_priority_fee_override(
            mint,
            pool,
            creator,
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
        creator: &Pubkey,
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
            creator,
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
        creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        _price: f64,
        use_idempotent: Option<bool>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.buy_for_user_with_priority_fee_override(
            buyer,
            mint,
            pool,
            creator,
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
        creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        _price: f64,
        use_idempotent: Option<bool>,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .context("failed to fetch pool state for pump.swap buy")?;

        let base_program = self
            .resolve_pool_mint_token_program(&state, mint, "pump.swap buy")
            .await
            .context("failed to resolve base token program for pump.swap buy")?;
        let quote_program = self
            .sol
            .get_token_program_id(&WSOL_MINT)
            .await
            .context("failed to resolve quote token program for pump.swap buy")?;

        let slippage_pct = Self::normalize_slippage(slippage);
        let sol_amount_lamports = (sol_amount_in * 1e9).ceil() as u64;
        anyhow::ensure!(sol_amount_lamports > 0, "pump.swap buy amount is too small");
        let max_sol_cost = ((sol_amount_lamports as f64) * (1.0 + slippage_pct)).ceil() as u64;

        let (creator_vault, creator_vault_ata) = self
            .derive_creator_vault(creator)
            .await
            .context("failed to derive creator vault for pump.swap buy")?;
        let user_volume_accu = self
            .derive_uv_accu(&buyer)
            .await
            .context("failed to derive user volume accumulator for pump.swap buy")?;

        let mut ixs = vec![];

        if use_idempotent.unwrap_or(true) {
            // Base mint ATA
            ixs.push(create_associated_token_account_idempotent(
                &buyer,
                &buyer,
                mint,
                &base_program,
            ));
            // WSOL ATA
            ixs.push(create_associated_token_account_idempotent(
                &buyer,
                &buyer,
                &WSOL_MINT,
                &quote_program,
            ));
        } else {
            ixs.push(create_associated_token_account(
                &buyer,
                &buyer,
                mint,
                &base_program,
            ));
            ixs.push(create_associated_token_account(
                &buyer,
                &buyer,
                &WSOL_MINT,
                &quote_program,
            ));
        }

        let wsol_ata = if quote_program == TOKEN_PROGRAM_ID {
            self.sol.get_ata_for_token(&buyer, &WSOL_MINT)
        } else if quote_program == TOKEN_2022_PROGRAM_ID {
            self.sol.get_ata_for_token2022(&buyer, &WSOL_MINT)
        } else {
            anyhow::bail!(
                "unsupported quote token program for pump.swap buy: {}",
                quote_program
            );
        };

        let associated_user = if base_program == TOKEN_PROGRAM_ID {
            self.sol.get_ata_for_token(&buyer, mint)
        } else {
            self.sol.get_ata_for_token2022(&buyer, mint)
        };
        let (protocol_fee_recipient, protocol_fee_recipient_ata) = self
            .protocol_fee_accounts_for_pool(&state, &quote_program)
            .await?;

        ixs.push(system_instruction_if::transfer(
            &buyer,
            &wsol_ata,
            max_sol_cost,
        ));

        ixs.push(sync_native(&quote_program, &wsol_ata)?);

        let mut accs = vec![
            AccountMeta::new(*pool, false), // pool must be mutable now
            AccountMeta::new(buyer, true),
            AccountMeta::new_readonly(GLOBAL_CONFIG_PUB, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(WSOL_MINT, false),
            AccountMeta::new(associated_user, false),
            AccountMeta::new(wsol_ata, false),
            AccountMeta::new(
                Address::from(state.pool_base_token_account.to_bytes()),
                false,
            ),
            AccountMeta::new(
                Address::from(state.pool_quote_token_account.to_bytes()),
                false,
            ),
            AccountMeta::new_readonly(protocol_fee_recipient, false),
            AccountMeta::new(protocol_fee_recipient_ata, false),
            // Base token program (may be Token2022)
            AccountMeta::new_readonly(base_program, false),
            // Quote token program (WSOL – legacy)
            AccountMeta::new_readonly(quote_program, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PUMP_SWAP_ID, false),
            AccountMeta::new(creator_vault_ata, false),
            AccountMeta::new_readonly(creator_vault, false),
            AccountMeta::new(GLOBAL_VOLUME_ACCUMULATOR, false),
            AccountMeta::new(user_volume_accu, false),
            AccountMeta::new_readonly(FEE_CONFIG, false),
            AccountMeta::new_readonly(FEE_PROGRAM, false),
        ];
        accs.extend(
            self.buy_remaining_accounts_for_pool(&state, pool, &user_volume_accu, &quote_program)
                .await
                .context("failed to derive pump.swap buy remaining accounts")?,
        );

        let recent_fees = self
            .sol
            .resolve_priority_fee(
                priority_fee_override,
                &accs.iter().map(|acc| acc.pubkey).collect::<Vec<Pubkey>>(),
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            )
            .await
            .context("failed to resolve priority fee for pump.swap buy")?;

        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let token_amount_out = self
            .quote_buy_base_amount_out_raw(&buyer, &state, &ixs, &accs, pool, sol_amount_lamports)
            .await
            .with_context(|| {
                format!(
                    "failed to quote pump.swap buy output for pool {} and spend {} lamports",
                    pool, sol_amount_lamports
                )
            });
        let data = match token_amount_out {
            Ok(token_amount_out) => {
                anyhow::ensure!(
                    token_amount_out > 0,
                    "pump.swap buy quote returned zero tokens"
                );
                let quote_data =
                    Self::encode_buy_instruction_data(token_amount_out, max_sol_cost, true);
                let mut quote_ixs = ixs.clone();
                quote_ixs.push(Instruction {
                    program_id: PUMP_SWAP_ID,
                    accounts: accs.clone(),
                    data: quote_data,
                });
                let simulated_base_amount_out = self
                    .simulate_buy_output_raw(&buyer, &quote_ixs, pool, "buy")
                    .await
                    .with_context(|| {
                        format!(
                            "failed to validate pump.swap buy output for pool {} and spend {} lamports",
                            pool, max_sol_cost
                        )
                    })?;
                log!(
                    cc::LIGHT_CYAN,
                    "pump.swap buy spend={} max_cost={} quoted_out={} simulated_out={}",
                    sol_amount_lamports,
                    max_sol_cost,
                    token_amount_out,
                    simulated_base_amount_out
                );
                Self::encode_buy_instruction_data(token_amount_out, max_sol_cost, true)
            }
            Err(error) => {
                warn!(
                    "pump.swap quote-first buy path failed for pool {} spend {} max_cost {}: {}; retrying with buy_exact_quote_in fallback",
                    pool, sol_amount_lamports, max_sol_cost, error
                );
                let mut quote_ixs = ixs.clone();
                quote_ixs.push(Instruction {
                    program_id: PUMP_SWAP_ID,
                    accounts: accs.clone(),
                    data: Self::encode_buy_exact_quote_in_instruction_data(max_sol_cost, 1, true),
                });
                let simulated_base_amount_out = self
                    .simulate_buy_output_raw(&buyer, &quote_ixs, pool, "buy_exact_quote_in")
                    .await
                    .with_context(|| {
                        format!(
                            "failed to validate pump.swap buy_exact_quote_in fallback for pool {} and spend {} lamports",
                            pool, max_sol_cost
                        )
                    })?;
                anyhow::ensure!(
                    simulated_base_amount_out > 0,
                    "pump.swap buy_exact_quote_in fallback returned zero tokens"
                );
                let min_base_amount_out =
                    (((simulated_base_amount_out as f64) * (1.0 - slippage_pct)).floor() as u64)
                        .max(1);
                log!(
                    cc::LIGHT_CYAN,
                    "pump.swap buy_exact_quote_in fallback spend={} max_cost={} simulated_out={} min_out={}",
                    sol_amount_lamports,
                    max_sol_cost,
                    simulated_base_amount_out,
                    min_base_amount_out
                );
                Self::encode_buy_exact_quote_in_instruction_data(
                    max_sol_cost,
                    min_base_amount_out,
                    true,
                )
            }
        };

        ixs.push(Instruction {
            program_id: PUMP_SWAP_ID,
            accounts: accs,
            data,
        });

        Ok((ixs, recent_fees))
    }

    pub async fn sell(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        price: f64,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.sell_with_priority_fee_override(mint, pool, creator, sell_pct, slippage, price, None)
            .await
    }

    pub async fn sell_with_priority_fee_override(
        &self,
        mint: &Pubkey,
        pool: &Pubkey,
        creator: &Pubkey,
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
            creator,
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
        creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        _price: f64,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.sell_for_user_with_priority_fee_override(
            buyer, mint, pool, creator, sell_pct, slippage, _price, None,
        )
        .await
    }

    pub async fn sell_for_user_with_priority_fee_override(
        &self,
        buyer: &Pubkey,
        mint: &Pubkey,
        pool: &Pubkey,
        creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        _price: f64,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .context("failed to fetch pool state for pump.swap sell")?;
        let program = self
            .resolve_pool_mint_token_program(&state, mint, "pump.swap sell")
            .await
            .context("failed to resolve token program for pump.swap sell")?;
        let quote_program = self
            .sol
            .get_token_program_id(&WSOL_MINT)
            .await
            .context("failed to resolve quote token program for pump.swap sell")?;
        let (creator_vault, creator_vault_ata) = self
            .derive_creator_vault(creator)
            .await
            .context("failed to derive creator vault for pump.swap sell")?;
        let associated_user = if program == TOKEN_PROGRAM_ID {
            self.sol.get_ata_for_token(&buyer, mint)
        } else {
            self.sol.get_ata_for_token2022(&buyer, mint)
        };

        let slippage_pct = Self::normalize_slippage(slippage);
        let (token_balance_raw, _token_decimals) = self
            .sol
            .get_token_balance_raw_from_ata(&associated_user)
            .await
            .context("failed to fetch token balance for pump.swap sell")?;
        anyhow::ensure!(token_balance_raw > 0, "no token balance for pump.swap sell");
        let sell_pct = sell_pct.clamp(1, 100);
        let token_amount_out = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            token_amount_out > 0,
            "pump.swap sell amount is too small for requested percentage"
        );

        let mut ixs = vec![];
        ixs.push(create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &quote_program,
        ));
        let wsol_ata = if quote_program == TOKEN_PROGRAM_ID {
            self.sol.get_ata_for_token(&buyer, &WSOL_MINT)
        } else if quote_program == TOKEN_2022_PROGRAM_ID {
            self.sol.get_ata_for_token2022(&buyer, &WSOL_MINT)
        } else {
            anyhow::bail!(
                "unsupported quote token program for pump.swap sell: {}",
                quote_program
            );
        };
        let user_volume_accu = self
            .derive_uv_accu(&buyer)
            .await
            .context("failed to derive user volume accumulator for pump.swap sell")?;
        let (protocol_fee_recipient, protocol_fee_recipient_ata) = self
            .protocol_fee_accounts_for_pool(&state, &quote_program)
            .await?;
        let mut accs = vec![
            AccountMeta::new(*pool, false), // pool must be mutable now
            AccountMeta::new(buyer, true),
            AccountMeta::new_readonly(GLOBAL_CONFIG_PUB, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(WSOL_MINT, false),
            AccountMeta::new(associated_user, false),
            AccountMeta::new(wsol_ata, false),
            AccountMeta::new(
                Address::from(state.pool_base_token_account.to_bytes()),
                false,
            ),
            AccountMeta::new(
                Address::from(state.pool_quote_token_account.to_bytes()),
                false,
            ),
            AccountMeta::new_readonly(protocol_fee_recipient, false),
            AccountMeta::new(protocol_fee_recipient_ata, false),
            AccountMeta::new_readonly(program, false),
            AccountMeta::new_readonly(quote_program, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PUMP_SWAP_ID, false),
            AccountMeta::new(creator_vault_ata, false),
            AccountMeta::new_readonly(creator_vault, false),
            AccountMeta::new_readonly(FEE_CONFIG, false),
            AccountMeta::new_readonly(FEE_PROGRAM, false),
        ];
        accs.extend(
            self.sell_remaining_accounts_for_pool(&state, pool, &user_volume_accu, &quote_program)
                .await
                .context("failed to derive pump.swap sell remaining accounts")?,
        );
        let recent_fees = self
            .sol
            .resolve_priority_fee(
                priority_fee_override,
                &accs.iter().map(|acc| acc.pubkey).collect::<Vec<Pubkey>>(),
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            )
            .await
            .context("failed to resolve priority fee for pump.swap sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);
        let quote_data = Self::encode_sell_instruction_data(token_amount_out, 1);
        let mut quote_ixs = ixs.clone();
        quote_ixs.push(Instruction {
            program_id: PUMP_SWAP_ID,
            accounts: accs.clone(),
            data: quote_data,
        });
        let simulated_quote_amount_out = self
            .simulate_sell_output_raw(&buyer, &quote_ixs, pool, token_amount_out)
            .await
            .with_context(|| {
                format!(
                    "failed to validate pump.swap sell output for pool {} and base amount {}",
                    pool, token_amount_out
                )
            })?;
        let min_sol_output = Self::apply_sell_slippage(simulated_quote_amount_out, slippage_pct);
        log!(
            cc::LIGHT_CYAN,
            "pump.swap sell amount_in={} quoted_out={} min_out={}",
            token_amount_out,
            simulated_quote_amount_out,
            min_sol_output
        );
        let data = Self::encode_sell_instruction_data(token_amount_out, min_sol_output);

        ixs.push(Instruction {
            program_id: PUMP_SWAP_ID,
            accounts: accs,
            data,
        });

        if sell_pct >= 100 && self.account_exists(&associated_user).await? {
            let close_token_ix =
                self.sol
                    .close_token_account_ix(&program, &associated_user, &buyer, &buyer)?;
            ixs.push(close_token_ix);
        }

        let close_wsol_ix =
            self.sol
                .close_token_account_ix(&quote_program, &wsol_ata, &buyer, &buyer)?;
        ixs.push(close_wsol_ix);

        Ok((ixs, recent_fees))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    use borsh010::BorshSerialize as BorshSerializeV0;
    use std::str::FromStr;

    fn encode_fixture_event<T: BorshSerializeV0>(discriminator: &[u8; 8], event: &T) -> String {
        let mut payload = Vec::with_capacity(1024);
        payload.extend_from_slice(discriminator);
        event
            .serialize(&mut payload)
            .expect("fixture event serialization must succeed");
        B64.encode(payload)
    }

    fn test_pump_swap() -> PumpSwap {
        let keypair = Arc::new(Keypair::new());
        let sol = Arc::new(SolHook::new("http://127.0.0.1:8899".to_string()));
        PumpSwap::new(keypair, sol)
    }

    #[test]
    fn test_pump_swap_discriminators_match_idl() {
        assert_eq!(BUY_IX_DISCRIM, [102, 6, 61, 18, 1, 218, 235, 234]);
        assert_eq!(
            BUY_EXACT_QUOTE_IN_IX_DISCRIM,
            [198, 46, 21, 82, 180, 217, 232, 112]
        );
        assert_eq!(SELL_IX_DISCRIM, [51, 230, 133, 164, 1, 127, 131, 173]);

        assert_eq!(
            CREATE_POOL_EVENT_DISCRIM,
            [177, 49, 12, 210, 160, 118, 167, 116]
        );
        assert_eq!(BUY_EVENT_DISCRIM, [103, 244, 82, 31, 44, 245, 119, 119]);
        assert_eq!(SELL_EVENT_DISCRIM, [62, 47, 55, 10, 165, 3, 220, 42]);
        assert_eq!(DEPOSIT_EVENT_DISCRIM, [120, 248, 61, 83, 31, 142, 107, 144]);
        assert_eq!(WITHDRAW_EVENT_DISCRIM, [22, 9, 133, 26, 160, 44, 71, 192]);
    }

    #[test]
    fn test_pump_swap_instruction_data_encoding() {
        let buy_data = PumpSwap::encode_buy_instruction_data(1_234, 9_876, true);
        assert_eq!(&buy_data[..8], &BUY_IX_DISCRIM);
        assert_eq!(
            u64::from_le_bytes(buy_data[8..16].try_into().unwrap()),
            1_234
        );
        assert_eq!(
            u64::from_le_bytes(buy_data[16..24].try_into().unwrap()),
            9_876
        );
        assert_eq!(buy_data[24], 1);
        assert_eq!(buy_data.len(), 25);

        let buy_exact_quote_in_data =
            PumpSwap::encode_buy_exact_quote_in_instruction_data(9_876, 1_234, true);
        assert_eq!(
            &buy_exact_quote_in_data[..8],
            &BUY_EXACT_QUOTE_IN_IX_DISCRIM
        );
        assert_eq!(
            u64::from_le_bytes(buy_exact_quote_in_data[8..16].try_into().unwrap()),
            9_876
        );
        assert_eq!(
            u64::from_le_bytes(buy_exact_quote_in_data[16..24].try_into().unwrap()),
            1_234
        );
        assert_eq!(buy_exact_quote_in_data[24], 1);
        assert_eq!(buy_exact_quote_in_data.len(), 25);

        let sell_data = PumpSwap::encode_sell_instruction_data(777, 888);
        assert_eq!(&sell_data[..8], &SELL_IX_DISCRIM);
        assert_eq!(
            u64::from_le_bytes(sell_data[8..16].try_into().unwrap()),
            777
        );
        assert_eq!(
            u64::from_le_bytes(sell_data[16..24].try_into().unwrap()),
            888
        );
        assert_eq!(sell_data.len(), 24);
    }

    #[test]
    fn test_pump_swap_program_constants() {
        let expected_program =
            Pubkey::from_str("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA").unwrap();
        assert_eq!(PUMP_SWAP_ID, expected_program);
        assert_eq!(CREATOR_OFFSET, 11);
        assert_eq!(BASE_MINT_OFFSET, 43);
        assert_eq!(QUOTE_MINT_OFFSET, 75);
        assert_eq!(LP_MINT_OFFSET, 107);
    }

    #[test]
    fn test_pump_swap_normalize_slippage() {
        assert_eq!(PumpSwap::normalize_slippage(0.1), 0.1);
        assert_eq!(PumpSwap::normalize_slippage(10.0), 0.1);
        assert_eq!(PumpSwap::normalize_slippage(0.0), 0.0);
        assert_eq!(PumpSwap::normalize_slippage(-3.0), 0.0);
    }

    #[test]
    fn test_pump_swap_apply_buy_slippage() {
        assert_eq!(PumpSwap::apply_buy_slippage(1_000, 0.15), 850);
        assert_eq!(PumpSwap::apply_buy_slippage(1, 0.99), 0);
        assert_eq!(PumpSwap::apply_buy_slippage(0, 0.15), 0);
    }

    #[test]
    fn test_pump_swap_effective_buy_quote_uses_combined_fee_denominator() {
        let effective_quote = PumpSwap::effective_buy_quote_amount_in_raw(10_000, 125)
            .expect("effective quote math should succeed");
        assert_eq!(effective_quote, 9_876);
    }

    #[test]
    fn test_pump_swap_buy_quote_output_matches_sdk_rounding() {
        let base_amount_out = PumpSwap::quote_buy_base_amount_out_with_total_fee_bps(
            10_000,
            1_000_000_000,
            1_000_000_000,
            125,
        )
        .expect("buy quote math should succeed");
        assert_eq!(base_amount_out, 9_875);
    }

    #[test]
    fn test_pump_swap_fee_recipient_switches_for_mayhem_mode() {
        assert_eq!(
            PumpSwap::protocol_fee_recipient_for_mode(false),
            PROTOCOL_FEE_RECIP
        );
        assert_eq!(
            PumpSwap::protocol_fee_recipient_for_mode(true),
            MAYHEM_FEE_RECIPIENT
        );
    }

    #[test]
    fn test_pump_swap_mayhem_fee_recipient_ata_uses_wsol_owner_ata() {
        let pump_swap = test_pump_swap();
        let (recipient, ata) = pump_swap
            .protocol_fee_accounts_for_mode(true, &TOKEN_PROGRAM_ID)
            .expect("mayhem fee account derivation must succeed");
        assert_eq!(recipient, MAYHEM_FEE_RECIPIENT);
        assert_eq!(
            ata,
            pump_swap
                .sol
                .get_ata_for_token(&MAYHEM_FEE_RECIPIENT, &WSOL_MINT)
        );
    }

    #[test]
    fn test_pump_swap_price_from_reserves_math() {
        // 2 SOL reserves and 1000 token reserves => 0.002 SOL/token
        let price = PumpSwap::price_from_reserves(1_000_000_000, 2_000_000_000);
        assert!((price - 0.002).abs() < 1e-12);
    }

    #[test]
    fn test_pump_swap_price_from_create_falls_back_to_in_amounts() {
        let fixture = CreatePoolEvent {
            pool_base_amount: 0,
            pool_quote_amount: 0,
            base_amount_in: 1_000_000_000,  // 1000 tokens
            quote_amount_in: 2_000_000_000, // 2 SOL
            ..Default::default()
        };

        let price = PumpSwap::price_from_create(&fixture);
        assert!((price - 0.002).abs() < 1e-12);
    }

    #[test]
    fn test_pump_swap_price_from_create_zero_base_is_zero() {
        let fixture = CreatePoolEvent {
            base_amount_in: 0,
            pool_base_amount: 0,
            quote_amount_in: 5_000_000_000,
            pool_quote_amount: 5_000_000_000,
            ..Default::default()
        };

        let price = PumpSwap::price_from_create(&fixture);
        assert_eq!(price, 0.0);
    }

    #[test]
    fn test_pump_swap_pool_lookup_specs_only_scan_pools_involving_target_mint() {
        let mint = Pubkey::from_str("2m6ewwrcaGoVHdbNUAFFVXWho45GBZuyJa4gmL2opump").unwrap();
        let specs = PumpSwap::pool_lookup_specs(&mint);
        assert_eq!(
            specs,
            vec![
                PoolLookupSpec {
                    base_mint: Some(mint),
                    quote_mint: None,
                },
                PoolLookupSpec {
                    base_mint: None,
                    quote_mint: Some(mint),
                }
            ]
        );
    }

    #[test]
    fn test_pump_swap_token_account_for_pool_mint_matches_both_sides() {
        let base_mint = Pubkey::from_str("2m6ewwrcaGoVHdbNUAFFVXWho45GBZuyJa4gmL2opump").unwrap();
        let quote_mint = WSOL_MINT;
        let base_vault = Pubkey::from_str("FViA7K2ibwQr2ihpqvHMH3FdV9SvYndC3dhk7MLb9fZh").unwrap();
        let quote_vault = Pubkey::from_str("7BdgsMvLQZ8ZoPXE6NRSxY6QECVPq3LcC8cGCsQ3pump").unwrap();
        let state = Pool {
            base_mint: base_mint.to_bytes().into(),
            quote_mint: quote_mint.to_bytes().into(),
            pool_base_token_account: base_vault.to_bytes().into(),
            pool_quote_token_account: quote_vault.to_bytes().into(),
            ..Default::default()
        };

        assert_eq!(
            PumpSwap::token_account_for_pool_mint(&state, &base_mint),
            Some(base_vault)
        );
        assert_eq!(
            PumpSwap::token_account_for_pool_mint(&state, &quote_mint),
            Some(quote_vault)
        );
        assert_eq!(
            PumpSwap::token_account_for_pool_mint(
                &state,
                &Pubkey::from_str("9CBntp4DfyKQEU2ZF9sEQbubS62UYbWvgbrPQxDxpump").unwrap(),
            ),
            None
        );
    }

    #[test]
    fn test_pump_swap_liquidity_token_account_defaults_to_wsol_quote_pair() {
        let mint = Pubkey::from_str("2m6ewwrcaGoVHdbNUAFFVXWho45GBZuyJa4gmL2opump").unwrap();
        let usdc_mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let base_vault = Pubkey::from_str("FViA7K2ibwQr2ihpqvHMH3FdV9SvYndC3dhk7MLb9fZh").unwrap();
        let quote_vault = Pubkey::from_str("7BdgsMvLQZ8ZoPXE6NRSxY6QECVPq3LcC8cGCsQ3pump").unwrap();

        let wsol_pool = Pool {
            base_mint: mint.to_bytes().into(),
            quote_mint: WSOL_MINT.to_bytes().into(),
            pool_base_token_account: base_vault.to_bytes().into(),
            pool_quote_token_account: quote_vault.to_bytes().into(),
            ..Default::default()
        };
        assert_eq!(
            PumpSwap::liquidity_token_account_for_mint_pair(&wsol_pool, &mint, None),
            Some(quote_vault)
        );

        let usdc_pool = Pool {
            base_mint: mint.to_bytes().into(),
            quote_mint: usdc_mint.to_bytes().into(),
            pool_base_token_account: base_vault.to_bytes().into(),
            pool_quote_token_account: quote_vault.to_bytes().into(),
            ..Default::default()
        };
        assert_eq!(
            PumpSwap::liquidity_token_account_for_mint_pair(&usdc_pool, &mint, None),
            None
        );
    }

    #[test]
    fn test_pump_swap_liquidity_token_account_supports_reverse_orientation_and_explicit_quote() {
        let mint = Pubkey::from_str("2m6ewwrcaGoVHdbNUAFFVXWho45GBZuyJa4gmL2opump").unwrap();
        let usdc_mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let base_vault = Pubkey::from_str("FViA7K2ibwQr2ihpqvHMH3FdV9SvYndC3dhk7MLb9fZh").unwrap();
        let quote_vault = Pubkey::from_str("7BdgsMvLQZ8ZoPXE6NRSxY6QECVPq3LcC8cGCsQ3pump").unwrap();

        let reversed_wsol_pool = Pool {
            base_mint: WSOL_MINT.to_bytes().into(),
            quote_mint: mint.to_bytes().into(),
            pool_base_token_account: base_vault.to_bytes().into(),
            pool_quote_token_account: quote_vault.to_bytes().into(),
            ..Default::default()
        };
        assert_eq!(
            PumpSwap::liquidity_token_account_for_mint_pair(&reversed_wsol_pool, &mint, None),
            Some(base_vault)
        );

        let explicit_usdc_pool = Pool {
            base_mint: mint.to_bytes().into(),
            quote_mint: usdc_mint.to_bytes().into(),
            pool_base_token_account: base_vault.to_bytes().into(),
            pool_quote_token_account: quote_vault.to_bytes().into(),
            ..Default::default()
        };
        assert_eq!(
            PumpSwap::liquidity_token_account_for_mint_pair(
                &explicit_usdc_pool,
                &mint,
                Some(&usdc_mint),
            ),
            Some(quote_vault)
        );
    }

    #[test]
    fn test_pump_swap_parse_logs_ignores_invalid_payloads() {
        let logs = vec![
            "Program log: hello".to_string(),
            "Program data: not-base64".to_string(),
            format!("Program data: {}", B64.encode([1u8, 2, 3, 4])), // too short
        ];
        let events = PumpSwap::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_pump_swap_parse_logs_unknown_discriminator() {
        // Unknown 8-byte discriminator should classify as Unknown.
        let payload = [1u8, 1, 1, 1, 1, 1, 1, 1, 42, 42];
        let logs = vec![format!("Program data: {}", B64.encode(payload))];
        let events = PumpSwap::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], PumpSwapEvent::Unknown));
    }

    #[test]
    fn test_pump_swap_parse_logs_create_pool_fixture_decodes() {
        let fixture = CreatePoolEvent {
            timestamp: 1_735_000_010,
            index: 9,
            base_mint_decimals: 6,
            quote_mint_decimals: 9,
            base_amount_in: 206_900_000_000_000,
            quote_amount_in: 80_990_359_346,
            pool_base_amount: 206_900_000_000_000,
            pool_quote_amount: 80_990_359_346,
            minimum_liquidity: 1_000,
            initial_liquidity: 1_000,
            lp_token_amount_out: 1_000,
            pool_bump: 255,
            is_mayhem_mode: false,
            ..Default::default()
        };

        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&CREATE_POOL_EVENT_DISCRIM, &fixture)
        )];
        let events = PumpSwap::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);

        match &events[0] {
            PumpSwapEvent::CreatePool(Some(event)) => {
                assert_eq!(event.base_mint_decimals, fixture.base_mint_decimals);
                assert_eq!(event.quote_mint_decimals, fixture.quote_mint_decimals);
                assert_eq!(event.pool_quote_amount, fixture.pool_quote_amount);
                assert_eq!(event.pool_base_amount, fixture.pool_base_amount);
                assert_eq!(event.is_mayhem_mode, fixture.is_mayhem_mode);
            }
            _ => panic!("expected parsed PumpSwap create-pool event"),
        }
    }

    #[test]
    fn test_pump_swap_parse_logs_buy_fixture_decodes() {
        let fixture = BuyEvent {
            timestamp: 1_735_000_020,
            base_amount_out: 250_000_000,
            max_quote_amount_in: 900_000_000,
            pool_base_token_reserves: 1_000_000_000,
            pool_quote_token_reserves: 2_000_000_000,
            quote_amount_in: 500_000_000,
            min_base_amount_out: 200_000_000,
            ix_name: "buy".to_string(),
            ..Default::default()
        };

        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&BUY_EVENT_DISCRIM, &fixture)
        )];
        let events = PumpSwap::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);

        match &events[0] {
            PumpSwapEvent::Buy(Some(event)) => {
                assert_eq!(event.base_amount_out, fixture.base_amount_out);
                assert_eq!(event.quote_amount_in, fixture.quote_amount_in);
                assert_eq!(event.ix_name, "buy");
                assert_eq!(
                    event.pool_quote_token_reserves,
                    fixture.pool_quote_token_reserves
                );
            }
            _ => panic!("expected parsed PumpSwap buy event"),
        }
    }

    #[test]
    fn test_pump_swap_parse_logs_sell_fixture_decodes() {
        let fixture = SellEvent {
            timestamp: 1_735_000_030,
            base_amount_in: 120_000_000,
            min_quote_amount_out: 230_000_000,
            pool_base_token_reserves: 1_500_000_000,
            pool_quote_token_reserves: 3_000_000_000,
            quote_amount_out: 250_000_000,
            quote_amount_out_without_lp_fee: 255_000_000,
            user_quote_amount_out: 245_000_000,
            ..Default::default()
        };

        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&SELL_EVENT_DISCRIM, &fixture)
        )];
        let events = PumpSwap::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);

        match &events[0] {
            PumpSwapEvent::Sell(Some(event)) => {
                assert_eq!(event.base_amount_in, fixture.base_amount_in);
                assert_eq!(event.quote_amount_out, fixture.quote_amount_out);
                assert_eq!(
                    event.quote_amount_out_without_lp_fee,
                    fixture.quote_amount_out_without_lp_fee
                );
            }
            _ => panic!("expected parsed PumpSwap sell event"),
        }
    }
}
