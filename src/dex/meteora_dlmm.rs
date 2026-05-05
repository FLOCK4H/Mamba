use crate::core::sol::{
    DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SolHook, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, WSOL_MINT,
};
use crate::log;
use crate::utils::utils::decode_b64;
use crate::utils::writing::cc;
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
use spl_associated_token_account::instruction::{
    create_associated_token_account, create_associated_token_account_idempotent,
};
use spl_token::state::Account as SplTokenAccount;
use spl_token_2022::instruction::sync_native;
use spl_token_2022::state::Account as SplToken2022Account;
use std::{collections::BTreeSet, str::FromStr, sync::Arc, time::Duration};

pub const METEORA_DLMM_ID: Pubkey =
    Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
pub const MEMO_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

pub const SWAP2_IX_DISCRIM: [u8; 8] = [65, 75, 63, 76, 235, 91, 91, 136];
pub const LB_PAIR_DISCRIM: [u8; 8] = [33, 11, 49, 98, 181, 101, 177, 13];
pub const BIN_ARRAY_DISCRIM: [u8; 8] = [92, 142, 92, 220, 5, 148, 70, 181];
pub const LB_PAIR_CREATE_EVENT_DISCRIM: [u8; 8] = [185, 74, 252, 125, 27, 215, 188, 111];
pub const SWAP_EVENT_DISCRIM: [u8; 8] = [81, 108, 227, 190, 205, 208, 10, 196];

pub const SEARCH_FOR: &str = "Program data: ";
pub const BIN_ARRAY_SEED: &[u8] = b"bin_array";
pub const BITMAP_SEED: &[u8] = b"bitmap";
pub const ORACLE_SEED: &[u8] = b"oracle";
pub const EVENT_AUTHORITY_SEED: &[u8] = b"__event_authority";
pub const TOKEN_BADGE_SEED: &[u8] = b"token_badge";

pub const ILM_BASE_KEY: Pubkey =
    Pubkey::from_str_const("MFGQxwAmB91SwuYX36okv2Qmdc9aMuHTwWGUrp4AtB1");

pub const LB_PAIR_ACCOUNT_LEN: usize = 904;
pub const BIN_ARRAY_ACCOUNT_LEN: usize = 10136;
pub const LB_PAIR_ACTIVE_ID_OFFSET: usize = 76;
pub const LB_PAIR_BIN_STEP_OFFSET: usize = 80;
pub const LB_PAIR_STATUS_OFFSET: usize = 82;
pub const LB_PAIR_TOKEN_X_MINT_OFFSET: usize = 88;
pub const LB_PAIR_TOKEN_Y_MINT_OFFSET: usize = 120;
pub const LB_PAIR_RESERVE_X_OFFSET: usize = 152;
pub const LB_PAIR_RESERVE_Y_OFFSET: usize = 184;
pub const LB_PAIR_ORACLE_OFFSET: usize = 552;
pub const LB_PAIR_CREATOR_OFFSET: usize = 848;
pub const LB_PAIR_TOKEN_X_PROGRAM_FLAG_OFFSET: usize = 880;
pub const LB_PAIR_TOKEN_Y_PROGRAM_FLAG_OFFSET: usize = 881;

const MAX_BIN_PER_ARRAY: i32 = 70;
const DEFAULT_BIN_ARRAY_CANDIDATES: usize = 6;

#[derive(Debug, Clone)]
pub struct DlmmLbPairState {
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub oracle: Pubkey,
    pub active_id: i32,
    pub bin_step: u16,
    pub status: u8,
    pub creator: Pubkey,
    pub token_mint_x_program_flag: u8,
    pub token_mint_y_program_flag: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LbPairCreateEvent {
    pub raw_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwapEvent {
    pub raw_data: Vec<u8>,
}

#[derive(Debug)]
pub enum MeteoraDlmmEvent {
    LbPairCreate(Option<LbPairCreateEvent>),
    Swap(Option<SwapEvent>),
    Unknown,
}

#[derive(Clone)]
pub struct MeteoraDlmm {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
}

impl MeteoraDlmm {
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

    pub fn decode_lb_pair_account_data(data: &[u8]) -> anyhow::Result<DlmmLbPairState> {
        anyhow::ensure!(
            data.len() >= LB_PAIR_ACCOUNT_LEN,
            "meteora dlmm lb_pair account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == LB_PAIR_DISCRIM,
            "meteora dlmm lb_pair discriminator mismatch"
        );

        Ok(DlmmLbPairState {
            token_x_mint: Self::read_pubkey(data, LB_PAIR_TOKEN_X_MINT_OFFSET)?,
            token_y_mint: Self::read_pubkey(data, LB_PAIR_TOKEN_Y_MINT_OFFSET)?,
            reserve_x: Self::read_pubkey(data, LB_PAIR_RESERVE_X_OFFSET)?,
            reserve_y: Self::read_pubkey(data, LB_PAIR_RESERVE_Y_OFFSET)?,
            oracle: Self::read_pubkey(data, LB_PAIR_ORACLE_OFFSET)?,
            active_id: Self::read_i32(data, LB_PAIR_ACTIVE_ID_OFFSET)?,
            bin_step: Self::read_u16(data, LB_PAIR_BIN_STEP_OFFSET)?,
            status: data[LB_PAIR_STATUS_OFFSET],
            creator: Self::read_pubkey(data, LB_PAIR_CREATOR_OFFSET)?,
            token_mint_x_program_flag: data[LB_PAIR_TOKEN_X_PROGRAM_FLAG_OFFSET],
            token_mint_y_program_flag: data[LB_PAIR_TOKEN_Y_PROGRAM_FLAG_OFFSET],
        })
    }

    fn encode_swap2_instruction_data(amount_in: u64, min_amount_out: u64) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + 8 + 8 + 4);
        data.extend_from_slice(&SWAP2_IX_DISCRIM);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        // RemainingAccountsInfo { slices: vec![] } serialized with Borsh (u32 length = 0)
        data.extend_from_slice(&0u32.to_le_bytes());
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

    fn derive_bin_array_pda(pool: &Pubkey, bin_array_index: i64) -> Pubkey {
        Pubkey::find_program_address(
            &[
                BIN_ARRAY_SEED,
                pool.as_ref(),
                &bin_array_index.to_le_bytes(),
            ],
            &METEORA_DLMM_ID,
        )
        .0
    }

    fn derive_bitmap_extension_pda(pool: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[BITMAP_SEED, pool.as_ref()], &METEORA_DLMM_ID).0
    }

    pub fn derive_event_authority_pda() -> Pubkey {
        Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], &METEORA_DLMM_ID).0
    }

    fn bin_id_to_bin_array_index(bin_id: i32) -> i32 {
        let idx = bin_id / MAX_BIN_PER_ARRAY;
        let rem = bin_id % MAX_BIN_PER_ARRAY;
        if bin_id.is_negative() && rem != 0 {
            idx - 1
        } else {
            idx
        }
    }

    fn pool_non_wsol_mint(state: &DlmmLbPairState) -> Option<Pubkey> {
        if state.token_x_mint == WSOL_MINT && state.token_y_mint != WSOL_MINT {
            Some(state.token_y_mint)
        } else if state.token_y_mint == WSOL_MINT && state.token_x_mint != WSOL_MINT {
            Some(state.token_x_mint)
        } else {
            None
        }
    }

    fn validate_pool_contains_mint(state: &DlmmLbPairState, mint: &Pubkey) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.token_x_mint == *mint || state.token_y_mint == *mint,
            "meteora dlmm pool does not contain mint {}",
            mint
        );
        anyhow::ensure!(
            state.token_x_mint == WSOL_MINT || state.token_y_mint == WSOL_MINT,
            "meteora dlmm pool is not WSOL-quoted"
        );
        Ok(())
    }

    fn ata_for(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey, sol: &SolHook) -> Pubkey {
        if *token_program == TOKEN_PROGRAM_ID {
            sol.get_ata_for_token(owner, mint)
        } else {
            sol.get_ata_for_token2022(owner, mint)
        }
    }

    fn expected_token_program(flag: u8) -> anyhow::Result<Pubkey> {
        match flag {
            0 => Ok(TOKEN_PROGRAM_ID),
            1 => Ok(TOKEN_2022_PROGRAM_ID),
            _ => anyhow::bail!("invalid token-program flag value: {flag}"),
        }
    }

    async fn resolve_token_program(
        &self,
        mint: &Pubkey,
        declared_flag: u8,
        label: &str,
    ) -> anyhow::Result<Pubkey> {
        let rpc_program =
            self.sol.get_token_program_id(mint).await.with_context(|| {
                format!("failed to resolve token program for {label} mint {mint}")
            })?;
        if let Ok(expected) = Self::expected_token_program(declared_flag)
            && expected != rpc_program
        {
            log!(
                cc::LIGHT_YELLOW,
                "warning: meteora dlmm {} token-program flag mismatch for mint {} (flag={}, rpc={})",
                label,
                mint,
                declared_flag,
                rpc_program
            );
        }
        Ok(rpc_program)
    }

    async fn fetch_reserve_balance_raw(&self, reserve: &Pubkey) -> anyhow::Result<u64> {
        let balance = self
            .sol
            .rpc_client
            .get_token_account_balance_with_commitment(reserve, CommitmentConfig::confirmed())
            .await
            .with_context(|| format!("failed to fetch reserve balance for {}", reserve))?;
        Ok(balance.value.amount.parse::<u64>()?)
    }

    pub async fn fetch_wsol_liquidity_raw(&self, state: &DlmmLbPairState) -> anyhow::Result<u64> {
        let reserve = if state.token_x_mint == WSOL_MINT {
            state.reserve_x
        } else if state.token_y_mint == WSOL_MINT {
            state.reserve_y
        } else {
            anyhow::bail!("pool is not WSOL-quoted");
        };
        self.fetch_reserve_balance_raw(&reserve).await
    }

    fn price_per_lamport(active_id: i32, bin_step: u16) -> anyhow::Result<f64> {
        anyhow::ensure!(bin_step > 0, "meteora dlmm bin_step must be > 0");
        let base = 1.0 + (bin_step as f64 / 10_000.0);
        anyhow::ensure!(base.is_finite() && base > 0.0, "invalid bin-step base");
        let price = base.powi(active_id);
        anyhow::ensure!(price.is_finite() && price > 0.0, "invalid active-bin price");
        Ok(price)
    }

    pub fn price_from_state_with_decimals(
        state: &DlmmLbPairState,
        token_x_decimals: u8,
        token_y_decimals: u8,
    ) -> anyhow::Result<f64> {
        let price_per_lamport = Self::price_per_lamport(state.active_id, state.bin_step)?;
        let token_price_y_per_x =
            price_per_lamport * 10_f64.powi(token_x_decimals as i32 - token_y_decimals as i32);
        anyhow::ensure!(
            token_price_y_per_x.is_finite() && token_price_y_per_x > 0.0,
            "invalid meteora dlmm token price"
        );

        if state.token_y_mint == WSOL_MINT && state.token_x_mint != WSOL_MINT {
            Ok(token_price_y_per_x)
        } else if state.token_x_mint == WSOL_MINT && state.token_y_mint != WSOL_MINT {
            Ok(1.0 / token_price_y_per_x)
        } else {
            Ok(token_price_y_per_x)
        }
    }

    async fn build_bin_array_remaining_accounts(
        &self,
        pool: &Pubkey,
        state: &DlmmLbPairState,
        input_mint: &Pubkey,
    ) -> anyhow::Result<Vec<AccountMeta>> {
        let swap_for_y = state.token_x_mint == *input_mint;
        let direction: i32 = if swap_for_y { -1 } else { 1 };
        let start = Self::bin_id_to_bin_array_index(state.active_id);

        let mut out = Vec::with_capacity(DEFAULT_BIN_ARRAY_CANDIDATES);
        let mut seen = BTreeSet::new();

        for i in 0..DEFAULT_BIN_ARRAY_CANDIDATES {
            let idx = start.saturating_add(direction.saturating_mul(i as i32));
            let pda = Self::derive_bin_array_pda(pool, idx as i64);
            if self.account_exists(&pda).await? && seen.insert(pda) {
                out.push(AccountMeta::new(pda, false));
            }
        }

        if out.is_empty() {
            let reverse = -direction;
            for i in 0..DEFAULT_BIN_ARRAY_CANDIDATES {
                let idx = start.saturating_add(reverse.saturating_mul(i as i32));
                let pda = Self::derive_bin_array_pda(pool, idx as i64);
                if self.account_exists(&pda).await? && seen.insert(pda) {
                    out.push(AccountMeta::new(pda, false));
                }
            }
        }

        anyhow::ensure!(
            !out.is_empty(),
            "no bin arrays found for meteora dlmm pool {}",
            pool
        );
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

    pub fn parse_logs(
        logs: std::slice::Iter<'_, String>,
        _sig: Option<&String>,
    ) -> Vec<MeteoraDlmmEvent> {
        let mut events: Vec<MeteoraDlmmEvent> = Vec::new();
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

            if b64[..8] == LB_PAIR_CREATE_EVENT_DISCRIM {
                events.push(MeteoraDlmmEvent::LbPairCreate(Some(LbPairCreateEvent {
                    raw_data: b64[8..].to_vec(),
                })));
            } else if b64[..8] == SWAP_EVENT_DISCRIM {
                events.push(MeteoraDlmmEvent::Swap(Some(SwapEvent {
                    raw_data: b64[8..].to_vec(),
                })));
            } else {
                events.push(MeteoraDlmmEvent::Unknown);
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
                    for key in ["lbPair", "lb_pair", "pool", "pair"] {
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
        let program_id = METEORA_DLMM_ID.to_string();

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
        fn extract_user_token_in_account(
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
                        .and_then(|v| Pubkey::from_str(v).ok())
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let token_in_idx = *compiled.accounts.get(4)? as usize;
                    let account = account_keys.get(token_in_idx)?;
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
        let program_id = METEORA_DLMM_ID.to_string();

        let UiMessage::Parsed(msg) = &ui_tx.message else {
            return Ok(None);
        };
        let account_keys: Vec<&str> = msg
            .account_keys
            .iter()
            .map(|account| account.pubkey.as_str())
            .collect();

        let mut user_token_in: Option<Pubkey> = None;
        for ix in &msg.instructions {
            user_token_in = extract_user_token_in_account(ix, &program_id, &account_keys);
            if user_token_in.is_some() {
                break;
            }
        }

        if user_token_in.is_none()
            && let Some(meta) = tx.transaction.meta.as_ref()
            && let OptionSerializer::Some(inner_instructions) = &meta.inner_instructions
        {
            for inner in inner_instructions {
                for ix in &inner.instructions {
                    user_token_in = extract_user_token_in_account(ix, &program_id, &account_keys);
                    if user_token_in.is_some() {
                        break;
                    }
                }
                if user_token_in.is_some() {
                    break;
                }
            }
        }

        let Some(user_token_in) = user_token_in else {
            return Ok(None);
        };
        let token_account = self
            .sol
            .rpc_client
            .get_account_with_commitment(&user_token_in, CommitmentConfig::confirmed())
            .await?
            .value
            .ok_or(anyhow::anyhow!(
                "user token-in account {} not found",
                user_token_in
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

    pub async fn fetch_state(&self, pool: &Pubkey) -> anyhow::Result<DlmmLbPairState> {
        let data = self
            .sol
            .rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("meteora dlmm lb_pair account not found"))?
            .data;
        Self::decode_lb_pair_account_data(&data)
    }

    pub async fn fetch_price(&self, pool: &Pubkey) -> anyhow::Result<(DlmmLbPairState, f64)> {
        let state = self.fetch_state(pool).await?;
        let token_x_decimals = self
            .sol
            .get_token_decimals(&state.token_x_mint)
            .await
            .with_context(|| {
                format!(
                    "failed to fetch token-x decimals for mint {}",
                    state.token_x_mint
                )
            })?;
        let token_y_decimals = self
            .sol
            .get_token_decimals(&state.token_y_mint)
            .await
            .with_context(|| {
                format!(
                    "failed to fetch token-y decimals for mint {}",
                    state.token_y_mint
                )
            })?;
        let price =
            Self::price_from_state_with_decimals(&state, token_x_decimals, token_y_decimals)?;
        Ok((state, price))
    }

    pub async fn get_mint_from_pool(&self, pool: &Pubkey) -> anyhow::Result<Pubkey> {
        let state = self.fetch_state(pool).await?;
        if let Some(mint) = Self::pool_non_wsol_mint(&state) {
            return Ok(mint);
        }
        Ok(state.token_x_mint)
    }

    pub async fn find_pools_by_mint(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
    ) -> anyhow::Result<Vec<Pubkey>> {
        if let Some(quote) = quote_mint {
            let cfg_x = RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::DataSize(LB_PAIR_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        LB_PAIR_TOKEN_X_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        LB_PAIR_TOKEN_Y_MINT_OFFSET,
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
            let cfg_y = RpcProgramAccountsConfig {
                filters: Some(vec![
                    RpcFilterType::DataSize(LB_PAIR_ACCOUNT_LEN as u64),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        LB_PAIR_TOKEN_Y_MINT_OFFSET,
                        mint.as_ref(),
                    )),
                    RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                        LB_PAIR_TOKEN_X_MINT_OFFSET,
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

            let pairs_x = self
                .sol
                .rpc_client
                .get_program_ui_accounts_with_config(&METEORA_DLMM_ID, cfg_x)
                .await?;
            let pairs_y = self
                .sol
                .rpc_client
                .get_program_ui_accounts_with_config(&METEORA_DLMM_ID, cfg_y)
                .await?;

            let mut out = BTreeSet::new();
            for (pool, _) in pairs_x.into_iter().chain(pairs_y.into_iter()) {
                out.insert(pool);
            }
            return Ok(out.into_iter().collect());
        }

        let mut filters_x = vec![
            RpcFilterType::DataSize(LB_PAIR_ACCOUNT_LEN as u64),
            RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                LB_PAIR_TOKEN_X_MINT_OFFSET,
                mint.as_ref(),
            )),
        ];
        let mut filters_y = vec![
            RpcFilterType::DataSize(LB_PAIR_ACCOUNT_LEN as u64),
            RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                LB_PAIR_TOKEN_Y_MINT_OFFSET,
                mint.as_ref(),
            )),
        ];

        let cfg_x = RpcProgramAccountsConfig {
            filters: Some(std::mem::take(&mut filters_x)),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };
        let cfg_y = RpcProgramAccountsConfig {
            filters: Some(std::mem::take(&mut filters_y)),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };

        let pairs_x = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&METEORA_DLMM_ID, cfg_x)
            .await?;
        let pairs_y = self
            .sol
            .rpc_client
            .get_program_ui_accounts_with_config(&METEORA_DLMM_ID, cfg_y)
            .await?;

        let mut out = BTreeSet::new();
        for (pool, _) in pairs_x.into_iter().chain(pairs_y.into_iter()) {
            if let Some(quote) = quote_mint {
                let state = match self.fetch_state(&pool).await {
                    Ok(state) => state,
                    Err(_) => continue,
                };
                if state.token_x_mint != *quote && state.token_y_mint != *quote {
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
                RpcFilterType::DataSize(LB_PAIR_ACCOUNT_LEN as u64),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
                    LB_PAIR_CREATOR_OFFSET,
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
            .get_program_ui_accounts_with_config(&METEORA_DLMM_ID, cfg)
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
        anyhow::ensure!(price > 0.0, "meteora dlmm buy price must be > 0");
        anyhow::ensure!(sol_amount_in > 0.0, "meteora dlmm buy amount must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch meteora dlmm state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;

        let token_x_program = self
            .resolve_token_program(
                &state.token_x_mint,
                state.token_mint_x_program_flag,
                "token_x",
            )
            .await?;
        let token_y_program = self
            .resolve_token_program(
                &state.token_y_mint,
                state.token_mint_y_program_flag,
                "token_y",
            )
            .await?;

        let input_program = if state.token_x_mint == WSOL_MINT {
            token_x_program
        } else {
            token_y_program
        };
        let output_program = if state.token_x_mint == *mint {
            token_x_program
        } else {
            token_y_program
        };

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
        anyhow::ensure!(amount_in > 0, "meteora dlmm buy amount is too small");

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
        ixs.push(sync_native(&input_program, &input_ata)?);

        let swap_for_y = state.token_x_mint == WSOL_MINT;
        let (user_token_in, user_token_out) = if swap_for_y {
            (input_ata, output_ata)
        } else {
            (input_ata, output_ata)
        };

        let bitmap_extension = Self::derive_bitmap_extension_pda(pool);
        let bitmap_extension_account = if self.account_exists(&bitmap_extension).await? {
            bitmap_extension
        } else {
            METEORA_DLMM_ID
        };
        let event_authority = Self::derive_event_authority_pda();

        let mut accounts = vec![
            AccountMeta::new(*pool, false),
            AccountMeta::new_readonly(bitmap_extension_account, false),
            AccountMeta::new(state.reserve_x, false),
            AccountMeta::new(state.reserve_y, false),
            AccountMeta::new(user_token_in, false),
            AccountMeta::new(user_token_out, false),
            AccountMeta::new_readonly(state.token_x_mint, false),
            AccountMeta::new_readonly(state.token_y_mint, false),
            AccountMeta::new(state.oracle, false),
            AccountMeta::new(METEORA_DLMM_ID, false),
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(token_x_program, false),
            AccountMeta::new_readonly(token_y_program, false),
            AccountMeta::new_readonly(MEMO_PROGRAM_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DLMM_ID, false),
        ];

        let remaining_accounts = self
            .build_bin_array_remaining_accounts(pool, &state, &WSOL_MINT)
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
            .context("failed to resolve priority fee for meteora dlmm buy")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap2_instruction_data(amount_in, min_amount_out);
        ixs.push(Instruction {
            program_id: METEORA_DLMM_ID,
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
        anyhow::ensure!(price > 0.0, "meteora dlmm sell price must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch meteora dlmm state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;

        let token_x_program = self
            .resolve_token_program(
                &state.token_x_mint,
                state.token_mint_x_program_flag,
                "token_x",
            )
            .await?;
        let token_y_program = self
            .resolve_token_program(
                &state.token_y_mint,
                state.token_mint_y_program_flag,
                "token_y",
            )
            .await?;

        let input_is_x = state.token_x_mint == *mint;
        let input_program = if input_is_x {
            token_x_program
        } else {
            token_y_program
        };
        let output_program = if input_is_x {
            token_y_program
        } else {
            token_x_program
        };

        let input_ata = Self::ata_for(&buyer, mint, &input_program, &self.sol);
        let output_ata = Self::ata_for(&buyer, &WSOL_MINT, &output_program, &self.sol);

        let token_balance_raw = self
            .user_token_balance_raw(&buyer, mint)
            .await
            .context("failed to fetch token balance for meteora dlmm sell")?;
        anyhow::ensure!(
            token_balance_raw > 0,
            "no token balance for meteora dlmm sell"
        );

        let sell_pct = sell_pct.clamp(1, 100);
        let amount_in = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            amount_in > 0,
            "meteora dlmm sell amount is too small for requested percentage"
        );

        let mint_decimals = self
            .sol
            .get_token_decimals(mint)
            .await
            .with_context(|| format!("failed to resolve decimals for mint {}", mint))?;
        let slippage_pct = Self::normalize_slippage(slippage);
        let amount_in_ui = amount_in as f64 / 10_f64.powi(mint_decimals as i32);
        let min_sol_output = amount_in_ui * price * (1.0 - slippage_pct);
        let min_sol_output = (min_sol_output.max(0.0) * 1e9).floor() as u64;

        let mut ixs = vec![create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &output_program,
        )];

        let bitmap_extension = Self::derive_bitmap_extension_pda(pool);
        let bitmap_extension_account = if self.account_exists(&bitmap_extension).await? {
            bitmap_extension
        } else {
            METEORA_DLMM_ID
        };
        let event_authority = Self::derive_event_authority_pda();

        let mut accounts = vec![
            AccountMeta::new(*pool, false),
            AccountMeta::new_readonly(bitmap_extension_account, false),
            AccountMeta::new(state.reserve_x, false),
            AccountMeta::new(state.reserve_y, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new_readonly(state.token_x_mint, false),
            AccountMeta::new_readonly(state.token_y_mint, false),
            AccountMeta::new(state.oracle, false),
            AccountMeta::new(METEORA_DLMM_ID, false),
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(token_x_program, false),
            AccountMeta::new_readonly(token_y_program, false),
            AccountMeta::new_readonly(MEMO_PROGRAM_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DLMM_ID, false),
        ];

        let remaining_accounts = self
            .build_bin_array_remaining_accounts(pool, &state, mint)
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
            .context("failed to resolve priority fee for meteora dlmm sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap2_instruction_data(amount_in, min_sol_output);
        ixs.push(Instruction {
            program_id: METEORA_DLMM_ID,
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
    fn test_meteora_dlmm_discriminators_match_anchor_layout() {
        assert_eq!(SWAP2_IX_DISCRIM, anchor_discriminator("global", "swap2"));
        assert_eq!(
            LB_PAIR_CREATE_EVENT_DISCRIM,
            anchor_discriminator("event", "LbPairCreate")
        );
        assert_eq!(SWAP_EVENT_DISCRIM, anchor_discriminator("event", "Swap"));
        assert_eq!(LB_PAIR_DISCRIM, anchor_discriminator("account", "LbPair"));
        assert_eq!(
            BIN_ARRAY_DISCRIM,
            anchor_discriminator("account", "BinArray")
        );
    }

    #[test]
    fn test_meteora_dlmm_swap2_instruction_data_encoding() {
        let data = MeteoraDlmm::encode_swap2_instruction_data(1_000_000, 123_456);
        assert_eq!(data.len(), 28);
        assert_eq!(&data[..8], &SWAP2_IX_DISCRIM);
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            1_000_000
        );
        assert_eq!(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            123_456
        );
        assert_eq!(u32::from_le_bytes(data[24..28].try_into().unwrap()), 0u32);
    }

    #[test]
    fn test_meteora_dlmm_bin_id_to_bin_array_index_math() {
        assert_eq!(MeteoraDlmm::bin_id_to_bin_array_index(0), 0);
        assert_eq!(MeteoraDlmm::bin_id_to_bin_array_index(69), 0);
        assert_eq!(MeteoraDlmm::bin_id_to_bin_array_index(70), 1);
        assert_eq!(MeteoraDlmm::bin_id_to_bin_array_index(-1), -1);
        assert_eq!(MeteoraDlmm::bin_id_to_bin_array_index(-70), -1);
        assert_eq!(MeteoraDlmm::bin_id_to_bin_array_index(-71), -2);
    }

    #[test]
    fn test_meteora_dlmm_price_from_state_with_decimals() {
        let state = DlmmLbPairState {
            token_x_mint: Pubkey::from_str("B4rGSdcBrmLEPUQXpZa91PMsRE3GqNcjLd6EMvM3yaj2").unwrap(),
            token_y_mint: WSOL_MINT,
            reserve_x: Pubkey::new_unique(),
            reserve_y: Pubkey::new_unique(),
            oracle: Pubkey::new_unique(),
            active_id: 0,
            bin_step: 100,
            status: 0,
            creator: Pubkey::new_unique(),
            token_mint_x_program_flag: 0,
            token_mint_y_program_flag: 0,
        };
        let price = MeteoraDlmm::price_from_state_with_decimals(&state, 6, 9).unwrap();
        assert!(price > 0.0);
        assert!((price - 0.001).abs() < 1e-12);
    }

    #[test]
    fn test_meteora_dlmm_decode_lb_pair_fixture_layout_contract() {
        let fixture = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/external/upstreams/meteora_dlmm_sdk/commons/tests/fixtures/B5Eia4cE71tKuEDaqPHucJLG2fxySKyKzLMewd2nUvoc/lb_pair.bin"
        ));

        let state = MeteoraDlmm::decode_lb_pair_account_data(fixture).expect("fixture must decode");

        assert_eq!(
            state.token_x_mint,
            Pubkey::from_str("B4rGSdcBrmLEPUQXpZa91PMsRE3GqNcjLd6EMvM3yaj2").unwrap()
        );
        assert_eq!(state.token_y_mint, WSOL_MINT);
        assert_eq!(
            state.reserve_x,
            Pubkey::from_str("HmAJViUS3iMSzuedDs1z4QxAPitnK8oNC6dwAaNrRTBE").unwrap()
        );
        assert_eq!(
            state.reserve_y,
            Pubkey::from_str("2LAbjR3C5pWMVKh7HUWyEZ6wwubHwHB6B91vu5jaB3mr").unwrap()
        );
        assert_eq!(
            state.oracle,
            Pubkey::from_str("HeC6TwhrT9eusRp8wMWuswpMAp1eUmr4mUB5csYMPsjU").unwrap()
        );
        assert!(state.bin_step > 0);
    }

    #[test]
    fn test_meteora_dlmm_parse_logs_ignores_invalid_payloads() {
        let logs = vec![
            "Program data: not_base64".to_string(),
            "Program data: AQI=".to_string(),
        ];
        let events = MeteoraDlmm::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_meteora_dlmm_parse_logs_unknown_discriminator() {
        let payload = B64.encode([0u8; 24]);
        let logs = vec![format!("Program data: {payload}")];
        let events = MeteoraDlmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MeteoraDlmmEvent::Unknown));
    }

    #[test]
    fn test_meteora_dlmm_parse_logs_lb_pair_create_fixture() {
        let event_payload = vec![1u8, 2, 3, 4, 5, 6];
        let encoded = encode_fixture_event(&LB_PAIR_CREATE_EVENT_DISCRIM, &event_payload);
        let logs = vec![format!("Program data: {encoded}")];

        let events = MeteoraDlmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDlmmEvent::LbPairCreate(Some(event)) => {
                assert_eq!(event.raw_data, event_payload);
            }
            other => panic!("expected lb_pair_create event, got {other:?}"),
        }
    }

    #[test]
    fn test_meteora_dlmm_parse_logs_swap_fixture() {
        let event_payload = vec![9u8, 8, 7, 6];
        let encoded = encode_fixture_event(&SWAP_EVENT_DISCRIM, &event_payload);
        let logs = vec![format!("Program data: {encoded}")];

        let events = MeteoraDlmm::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDlmmEvent::Swap(Some(event)) => {
                assert_eq!(event.raw_data, event_payload);
            }
            other => panic!("expected swap event, got {other:?}"),
        }
    }

    #[test]
    fn test_meteora_dlmm_extract_pool_from_inner_instruction_fixture() {
        let pool = "HTvjzsfX3yU6BUodCjZ5vZkUrAxMDTrBs3CJaq43ashR";
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
                            "pubkey": METEORA_DLMM_ID.to_string(),
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
                                "programId": METEORA_DLMM_ID.to_string(),
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
            MeteoraDlmm::extract_pool_from_transaction(&tx),
            Some(Pubkey::from_str(pool).unwrap())
        );
    }
}
