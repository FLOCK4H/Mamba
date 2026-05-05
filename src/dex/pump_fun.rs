use crate::core::sol::TOKEN_PROGRAM_ID;
use crate::core::sol::{
    DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SYSTEM_PROGRAM, SolHook,
    TOKEN_2022_PROGRAM_ID, WSOL_MINT,
};
use crate::utils::utils::decode_b64;
use crate::utils::writing::cc;
use crate::{log, warn};
use anyhow::Context;
use borsh::{BorshDeserialize, BorshSerialize};
use borsh010::BorshDeserialize as BorshDeserializeV0;
use pump_fun_types::events::{CreateEvent, TradeEvent};
use reqwest::Client;
use reqwest::header;
use reqwest::header::HeaderMap;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
};
use solana_signer::Signer;
use spl_associated_token_account::instruction::{
    create_associated_token_account, create_associated_token_account_idempotent,
};
use spl_token::state::Account as SplTokenAccount;
use spl_token_2022::state::Account as SplToken2022Account;
use std::io::Cursor;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct BondingCurveAccount {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
    pub creator: Pubkey,
    pub is_mayhem_mode: bool,
    pub is_cashback_coin: bool,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[borsh(crate = "borsh")]
struct BondingCurveAccountRaw {
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
    pub creator: Pubkey,
}

#[derive(Debug, BorshDeserialize)]
#[borsh(crate = "borsh")]
struct PumpGlobalAccountState {
    #[allow(dead_code)]
    initialized: bool,
    #[allow(dead_code)]
    authority: Pubkey,
    #[allow(dead_code)]
    fee_recipient: Pubkey,
    #[allow(dead_code)]
    initial_virtual_token_reserves: u64,
    #[allow(dead_code)]
    initial_virtual_sol_reserves: u64,
    #[allow(dead_code)]
    initial_real_token_reserves: u64,
    #[allow(dead_code)]
    token_total_supply: u64,
    fee_basis_points: u64,
    #[allow(dead_code)]
    withdraw_authority: Pubkey,
    #[allow(dead_code)]
    enable_migrate: bool,
    #[allow(dead_code)]
    pool_migration_fee: u64,
    creator_fee_basis_points: u64,
    #[allow(dead_code)]
    fee_recipients: [Pubkey; 7],
    #[allow(dead_code)]
    set_creator_authority: Pubkey,
    #[allow(dead_code)]
    admin_set_creator_authority: Pubkey,
    #[allow(dead_code)]
    create_v2_enabled: bool,
    #[allow(dead_code)]
    whitelist_pda: Pubkey,
    #[allow(dead_code)]
    reserved_fee_recipient: Pubkey,
    #[allow(dead_code)]
    mayhem_mode_enabled: bool,
    #[allow(dead_code)]
    reserved_fee_recipients: [Pubkey; 7],
    #[allow(dead_code)]
    is_cashback_enabled: bool,
}

#[derive(Debug, BorshDeserialize)]
#[borsh(crate = "borsh")]
struct PumpFeeRatesState {
    #[allow(dead_code)]
    lp_fee_bps: u64,
    protocol_fee_bps: u64,
    creator_fee_bps: u64,
}

#[derive(Debug, BorshDeserialize)]
#[borsh(crate = "borsh")]
struct PumpFeeTierState {
    market_cap_lamports_threshold: u128,
    fees: PumpFeeRatesState,
}

#[derive(Debug, BorshDeserialize)]
#[borsh(crate = "borsh")]
struct PumpFeeConfigState {
    #[allow(dead_code)]
    bump: u8,
    #[allow(dead_code)]
    admin: Pubkey,
    #[allow(dead_code)]
    flat_fees: PumpFeeRatesState,
    fee_tiers: Vec<PumpFeeTierState>,
}

#[derive(Debug)]
pub enum PumpFunEvent {
    Trade(Option<TradeEvent>),
    Create(Option<CreateEvent>),
    Unknown,
}

pub const PUMP_FUN_ID: Pubkey =
    Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
pub const FEE_RECIPIENT: Pubkey =
    Pubkey::from_str_const("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV");
pub const EVENT_AUTHORITY: Pubkey =
    Pubkey::from_str_const("Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1");
pub const GLOBAL_VOLUME_ACCUMULATOR: Pubkey =
    Pubkey::from_str_const("Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y");
pub const GLOBAL: Pubkey = Pubkey::from_str_const("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf");
pub const FEE_PROGRAM: Pubkey =
    Pubkey::from_str_const("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
pub const FEE_CONFIG: Pubkey =
    Pubkey::from_str_const("8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt");
pub const MAYHEM_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MAyhSmzXzV1pTf7LsNkrNwkWKTo4ougAJ1PPg47MD4e");
pub const MAYHEM_FEE_RECIPIENT: Pubkey =
    Pubkey::from_str_const("GesfTA3X2arioaHp8bbKdjG9vJtskViWACZoYvxp4twS");
pub const BUYBACK_FEE_RECIPIENTS: [Pubkey; 8] = [
    Pubkey::from_str_const("5YxQFdt3Tr9zJLvkFccqXVUwhdTWJQc1fFg2YPbxvxeD"),
    Pubkey::from_str_const("9M4giFFMxmFGXtc3feFzRai56WbBqehoSeRE5GK7gf7"),
    Pubkey::from_str_const("GXPFM2caqTtQYC2cJ5yJRi9VDkpsYZXzYdwYpGnLmtDL"),
    Pubkey::from_str_const("3BpXnfJaUTiwXnJNe7Ej1rcbzqTTQUvLShZaWazebsVR"),
    Pubkey::from_str_const("5cjcW9wExnJJiqgLjq7DEG75Pm6JBgE1hNv4B2vHXUW6"),
    Pubkey::from_str_const("EHAAiTxcdDwQ3U4bU6YcMsQGaekdzLS3B5SmYo46kJtL"),
    Pubkey::from_str_const("5eHhjP8JaYkz83CWwvGU2uMUXefd3AazWGx4gpcuEEYD"),
    Pubkey::from_str_const("A7hAgCzFw14fejgCp387JUJRMNyz4j89JKnhtKU8piqW"),
];

pub const CREATE_DISCRIM: [u8; 8] = [27, 114, 169, 77, 222, 235, 99, 118];
pub const TRADE_DISCRIM: [u8; 8] = [189, 219, 127, 211, 78, 230, 97, 238];
pub const BUY_DISCRIM: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const BUY_EXACT_SOL_IN_DISCRIM: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];
pub const CREATE_V2_IX_DISCRIM: [u8; 8] = [214, 144, 76, 236, 95, 139, 49, 180];
pub const EXTEND_ACCOUNT_IX_DISCRIM: [u8; 8] = [234, 102, 194, 203, 150, 72, 62, 229];
pub const SELL_DISCRIM: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
pub const INIT_USER_VOLUME_ACCUMULATOR_DISCRIM: [u8; 8] = [94, 6, 202, 115, 255, 96, 232, 183];

pub const CREATE_SIG: &str = "G3K";
pub const TRADE_SIG: &str = "vdt";

pub const SEARCH_FOR: &str = "Program data: ";
pub const RAPID_LAUNCH_URI: &str = "https://rapidlaunch.io/";
pub const PUMP_FUN_URI: &str = "https://ipfs.io";
pub const TOTAL_SUPPLY: u64 = 1_000_000_000;
const LOW_SOL_POOL_REAL_RESERVES_THRESHOLD_LAMPORTS: u64 = 10_000_000; // 0.01 SOL
const LOW_SOL_POOL_BUY_BUFFER_LAMPORTS: u64 = 16;
const BUY_NETWORK_FEE_BUFFER_LAMPORTS: u64 = 10_000;
const ONE_BILLION_SUPPLY_RAW: u128 = 1_000_000_000_000_000;
const FEE_BPS_SCALE: u128 = 10_000;
const SLIPPAGE_SCALE_MILLIS: u128 = 1_000;

#[derive(Debug, Clone, Default)]
pub struct Socials {
    pub twitter: Option<String>,
    pub telegram: Option<String>,
    pub website: Option<String>,
}

#[derive(Clone)]
pub struct PumpFun {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
    client: Arc<Client>,
}

impl PumpFun {
    pub fn new(keypair: Arc<Keypair>, sol: Arc<SolHook>) -> Self {
        Self {
            keypair,
            sol,
            client: Arc::new(Client::new()),
        }
    }

    fn decode_bonding_curve_account_data(data: &[u8]) -> anyhow::Result<BondingCurveAccount> {
        if data.len() < 8 {
            anyhow::bail!("account too short");
        }

        let body = &data[8..];
        let mut cur = Cursor::new(body);
        let raw = BondingCurveAccountRaw::deserialize_reader(&mut cur)?;
        let cursor_pos = cur.position() as usize;
        let is_mayhem_mode = body.get(cursor_pos).copied().unwrap_or(0) != 0;
        let is_cashback_coin = body.get(cursor_pos + 1).copied().unwrap_or(0) != 0;

        Ok(BondingCurveAccount {
            virtual_token_reserves: raw.virtual_token_reserves,
            virtual_sol_reserves: raw.virtual_sol_reserves,
            real_token_reserves: raw.real_token_reserves,
            real_sol_reserves: raw.real_sol_reserves,
            token_total_supply: raw.token_total_supply,
            complete: raw.complete,
            creator: raw.creator,
            is_mayhem_mode,
            is_cashback_coin,
        })
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

    fn fee_recipient_for_mode(
        global_state: &PumpGlobalAccountState,
        is_mayhem_mode: bool,
    ) -> Pubkey {
        if is_mayhem_mode {
            if global_state.reserved_fee_recipient != Pubkey::default() {
                return global_state.reserved_fee_recipient;
            }
            global_state
                .reserved_fee_recipients
                .iter()
                .copied()
                .find(|recipient| *recipient != Pubkey::default())
                .unwrap_or(MAYHEM_FEE_RECIPIENT)
        } else if global_state.fee_recipient != Pubkey::default() {
            global_state.fee_recipient
        } else {
            global_state
                .fee_recipients
                .iter()
                .copied()
                .find(|recipient| *recipient != Pubkey::default())
                .unwrap_or(FEE_RECIPIENT)
        }
    }

    fn buyback_fee_recipient(user: &Pubkey, mint: &Pubkey) -> Pubkey {
        let user_bytes = user.to_bytes();
        let mint_bytes = mint.to_bytes();
        let index = usize::from(user_bytes[0] ^ mint_bytes[0]) % BUYBACK_FEE_RECIPIENTS.len();
        BUYBACK_FEE_RECIPIENTS[index]
    }

    async fn resolve_mint_token_program_with_legacy_fallback(
        &self,
        mint: &Pubkey,
        context: &str,
    ) -> Pubkey {
        match self.sol.get_token_program_id(mint).await {
            Ok(program) => program,
            Err(error) => {
                log!(
                    cc::LIGHT_YELLOW,
                    "{}: failed to resolve token program for mint {}; assuming legacy SPL Token program: {}",
                    context,
                    mint,
                    error
                );
                TOKEN_PROGRAM_ID
            }
        }
    }

    pub async fn get_market_cap(trade: &TradeEvent, sol_price_usd: f64) -> anyhow::Result<f64> {
        let price = Self::get_price(trade);
        let price_in_usd = price * sol_price_usd;
        Ok(1000000000.0 * price_in_usd)
    }

    pub async fn derive_bonding_curve(mint: &Pubkey) -> anyhow::Result<Pubkey> {
        let (pda, _) =
            Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &PUMP_FUN_ID);
        Ok(pda)
    }

    pub async fn fetch_state(&self, bonding_curve: &Pubkey) -> anyhow::Result<BondingCurveAccount> {
        let data = self
            .sol
            .rpc_client
            .get_account_with_commitment(bonding_curve, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("account not found"))?
            .data;
        Self::decode_bonding_curve_account_data(&data)
    }

    async fn fetch_global_state(&self) -> anyhow::Result<PumpGlobalAccountState> {
        let data = self
            .sol
            .rpc_client
            .get_account_with_commitment(&GLOBAL, CommitmentConfig::processed())
            .await?
            .value
            .ok_or_else(|| anyhow::anyhow!("pump.fun global account not found"))?
            .data;
        if data.len() < 8 {
            anyhow::bail!("pump.fun global account too short");
        }
        let mut cur = Cursor::new(&data[8..]);
        PumpGlobalAccountState::deserialize_reader(&mut cur)
            .context("failed to decode pump.fun global account")
    }

    async fn fetch_fee_config(&self) -> anyhow::Result<Option<PumpFeeConfigState>> {
        let account = self
            .sol
            .rpc_client
            .get_account_with_commitment(&FEE_CONFIG, CommitmentConfig::processed())
            .await?
            .value;
        let Some(account) = account else {
            return Ok(None);
        };
        if account.data.len() < 8 {
            return Ok(None);
        }
        let mut cur = Cursor::new(&account.data[8..]);
        let decoded = PumpFeeConfigState::deserialize_reader(&mut cur)
            .context("failed to decode pump.fun fee config")?;
        Ok(Some(decoded))
    }

    pub async fn get_creator(&self, bonding_curve: &Pubkey) -> anyhow::Result<Pubkey> {
        let state = self.fetch_state(bonding_curve).await?;
        Ok(state.creator)
    }

    pub async fn find_pool_by_mint_with_min_liquidity(
        &self,
        mint: &Pubkey,
        quote_mint: Option<&Pubkey>,
        min_liquidity_raw: u64,
    ) -> anyhow::Result<Option<Pubkey>> {
        if let Some(quote) = quote_mint
            && *quote != WSOL_MINT
        {
            return Ok(None);
        }

        let bonding_curve = Self::derive_bonding_curve(mint).await?;
        let state = match self.fetch_state(&bonding_curve).await {
            Ok(state) => state,
            Err(_) => return Ok(None),
        };

        if state.complete || state.real_sol_reserves < min_liquidity_raw {
            return Ok(None);
        }

        Ok(Some(bonding_curve))
    }

    pub fn parse_logs(
        logs: std::slice::Iter<'_, String>,
        sig: Option<&String>,
    ) -> Vec<PumpFunEvent> {
        let mut events: Vec<PumpFunEvent> = Vec::new();
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
                if b64[..8] == TRADE_DISCRIM {
                    let mut cur = Cursor::new(&b64[8..]);
                    let dtx: Result<TradeEvent, _> =
                        BorshDeserializeV0::deserialize_reader(&mut cur);
                    match dtx {
                        Ok(dtx) => {
                            events.push(PumpFunEvent::Trade(Some(dtx)));
                        }
                        Err(e) => {
                            warn!(
                                "Error deserializing trade event {:?}: {e}",
                                sig.unwrap_or(&"".to_string())
                            );
                        }
                    }
                } else if b64[..8] == CREATE_DISCRIM {
                    let mut cur = Cursor::new(&b64[8..]);
                    let dtx: Result<CreateEvent, _> =
                        BorshDeserializeV0::deserialize_reader(&mut cur);
                    match dtx {
                        Ok(dtx) => {
                            events.push(PumpFunEvent::Create(Some(dtx)));
                        }
                        Err(e) => {
                            warn!(
                                "Error deserializing create event {:?}: {e}",
                                sig.unwrap_or(&"".to_string())
                            );
                        }
                    }
                }
            }
        }
        events
    }

    pub fn get_price(event: &TradeEvent) -> f64 {
        let vsr = event.virtual_sol_reserves as f64 / 1e9;
        let vtr = event.virtual_token_reserves as f64 / 1e6;
        vsr / vtr
    }

    pub fn get_open_price(event: &CreateEvent) -> f64 {
        let vsr = event.virtual_sol_reserves as f64 / 1e9;
        let vtr = event.virtual_token_reserves as f64 / 1e6;
        vsr / vtr
    }

    pub async fn fetch_price(
        &self,
        bonding_curve: &Pubkey,
    ) -> anyhow::Result<(BondingCurveAccount, f64)> {
        let state = self.fetch_state(bonding_curve).await?;
        let vsr = state.virtual_sol_reserves as f64 / 1e9;
        let vtr = state.virtual_token_reserves as f64 / 1e6;
        let price = vsr / vtr;
        Ok((state, price))
    }

    pub fn user_volume_accu_pda(&self, user: &Pubkey) -> Pubkey {
        let (pda, _) = Pubkey::find_program_address(
            &[b"user_volume_accumulator", user.as_ref()],
            &PUMP_FUN_ID,
        );
        pda
    }

    pub fn creator_vault_pda(&self, creator: &Pubkey) -> Pubkey {
        let (pda, _) =
            Pubkey::find_program_address(&[b"creator-vault", creator.as_ref()], &PUMP_FUN_ID);
        pda
    }

    fn ceil_div_u128(value: u128, divisor: u128) -> anyhow::Result<u128> {
        anyhow::ensure!(divisor > 0, "division by zero");
        Ok(value.div_ceil(divisor))
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

    fn fee_amount_lamports(amount: u128, fee_bps: u64) -> anyhow::Result<u128> {
        Self::ceil_div_u128(
            amount
                .checked_mul(u128::from(fee_bps))
                .context("pump.fun fee multiplication overflow")?,
            FEE_BPS_SCALE,
        )
    }

    fn sell_slippage_millis(slippage_pct: f64) -> u128 {
        ((slippage_pct * SLIPPAGE_SCALE_MILLIS as f64).floor() as u128).min(999)
    }

    fn quote_fee_bps(
        global: &PumpGlobalAccountState,
        fee_config: Option<&PumpFeeConfigState>,
        curve_state: &BondingCurveAccount,
    ) -> anyhow::Result<(u64, u64)> {
        if let Some(cfg) = fee_config
            && let Some(first) = cfg.fee_tiers.first()
        {
            anyhow::ensure!(
                curve_state.virtual_token_reserves > 0,
                "pump.fun virtual_token_reserves is zero"
            );
            let mint_supply = if curve_state.is_mayhem_mode {
                u128::from(curve_state.token_total_supply)
            } else {
                ONE_BILLION_SUPPLY_RAW
            };
            let market_cap_lamports = Self::checked_mul_div_u128(
                u128::from(curve_state.virtual_sol_reserves),
                mint_supply,
                u128::from(curve_state.virtual_token_reserves),
                "pump.fun market cap",
            )?;
            let selected = if market_cap_lamports < first.market_cap_lamports_threshold {
                &first.fees
            } else {
                cfg.fee_tiers
                    .iter()
                    .rev()
                    .find(|tier| market_cap_lamports >= tier.market_cap_lamports_threshold)
                    .map(|tier| &tier.fees)
                    .unwrap_or(&first.fees)
            };
            let creator_fee_bps = if curve_state.creator == Pubkey::default() {
                0
            } else {
                selected.creator_fee_bps
            };
            return Ok((selected.protocol_fee_bps, creator_fee_bps));
        }

        let creator_fee_bps = if curve_state.creator == Pubkey::default() {
            0
        } else {
            global.creator_fee_basis_points
        };
        Ok((global.fee_basis_points, creator_fee_bps))
    }

    fn quote_buy_token_amount_out_raw(
        global: &PumpGlobalAccountState,
        fee_config: Option<&PumpFeeConfigState>,
        curve_state: &BondingCurveAccount,
        sol_amount_lamports: u64,
    ) -> anyhow::Result<u64> {
        if sol_amount_lamports == 0 {
            return Ok(0);
        }
        anyhow::ensure!(
            curve_state.virtual_sol_reserves > 0 && curve_state.virtual_token_reserves > 0,
            "pump.fun reserves are invalid"
        );
        let (protocol_fee_bps, creator_fee_bps) =
            Self::quote_fee_bps(global, fee_config, curve_state)?;
        let total_fee_bps = u128::from(protocol_fee_bps)
            .checked_add(u128::from(creator_fee_bps))
            .context("pump.fun total fee overflow")?;
        let spend_lamports = u128::from(sol_amount_lamports);
        let spendable = spend_lamports
            .saturating_sub(1)
            .checked_mul(FEE_BPS_SCALE)
            .context("pump.fun buy spendable overflow")?
            .checked_div(FEE_BPS_SCALE + total_fee_bps)
            .context("pump.fun buy spendable division failed")?;
        let tokens_out = Self::checked_mul_div_u128(
            spendable,
            u128::from(curve_state.virtual_token_reserves),
            u128::from(curve_state.virtual_sol_reserves)
                .checked_add(spendable)
                .context("pump.fun buy denominator overflow")?,
            "pump.fun buy quote",
        )?;
        let capped = tokens_out.min(u128::from(curve_state.real_token_reserves));
        anyhow::ensure!(
            capped <= u128::from(u64::MAX),
            "pump.fun buy quote overflow u64"
        );
        Ok(capped as u64)
    }

    fn quote_sell_sol_output_raw(
        global: &PumpGlobalAccountState,
        fee_config: Option<&PumpFeeConfigState>,
        curve_state: &BondingCurveAccount,
        token_amount_raw: u64,
    ) -> anyhow::Result<u64> {
        if token_amount_raw == 0 {
            return Ok(0);
        }
        anyhow::ensure!(
            curve_state.virtual_token_reserves > 0,
            "pump.fun virtual_token_reserves is zero"
        );
        let gross_output = Self::checked_mul_div_u128(
            u128::from(token_amount_raw),
            u128::from(curve_state.virtual_sol_reserves),
            u128::from(curve_state.virtual_token_reserves)
                .checked_add(u128::from(token_amount_raw))
                .context("pump.fun sell denominator overflow")?,
            "pump.fun sell quote",
        )?;
        let (protocol_fee_bps, creator_fee_bps) =
            Self::quote_fee_bps(global, fee_config, curve_state)?;
        let protocol_fee = Self::fee_amount_lamports(gross_output, protocol_fee_bps)?;
        let creator_fee = Self::fee_amount_lamports(gross_output, creator_fee_bps)?;
        let net_output = gross_output
            .checked_sub(protocol_fee)
            .and_then(|value| value.checked_sub(creator_fee))
            .context("pump.fun sell net output underflow")?;
        anyhow::ensure!(
            net_output <= u128::from(u64::MAX),
            "pump.fun sell quote overflow u64"
        );
        Ok(net_output as u64)
    }

    fn apply_sell_slippage(raw_output_lamports: u64, slippage_pct: f64) -> u64 {
        if raw_output_lamports == 0 {
            return 0;
        }
        let slippage_millis = Self::sell_slippage_millis(slippage_pct);
        let kept_millis = SLIPPAGE_SCALE_MILLIS.saturating_sub(slippage_millis);
        u128::from(raw_output_lamports)
            .saturating_mul(kept_millis)
            .checked_div(SLIPPAGE_SCALE_MILLIS)
            .unwrap_or(0)
            .min(u128::from(u64::MAX)) as u64
    }

    pub fn lamports_to_tokens(&self, lamports: f64, price: f64) -> u64 {
        let lamports = lamports / 1e9;
        let tokens = lamports / price;
        (tokens * 1e6) as u64
    }

    fn normalize_slippage(slippage: f64) -> f64 {
        let normalized = if slippage > 1.0 {
            slippage / 100.0
        } else {
            slippage
        };
        normalized.clamp(0.01, 0.99)
    }

    fn sol_to_lamports(sol_amount: f64) -> u64 {
        if !sol_amount.is_finite() || sol_amount <= 0.0 {
            return 0;
        }
        (sol_amount * 1e9).ceil() as u64
    }

    fn buy_max_sol_cost(
        sol_amount_lamports: u64,
        slippage_pct: f64,
        real_sol_reserves: u64,
    ) -> u64 {
        if sol_amount_lamports == 0 {
            return 0;
        }

        let slippage_bps = (slippage_pct * 10_000.0).ceil() as u128;
        let scale = 10_000u128;
        let scaled = (sol_amount_lamports as u128)
            .saturating_mul(scale.saturating_add(slippage_bps))
            .div_ceil(scale);
        let mut max_sol_cost = scaled.min(u64::MAX as u128) as u64;

        if real_sol_reserves <= LOW_SOL_POOL_REAL_RESERVES_THRESHOLD_LAMPORTS {
            // Very low-liquidity pools are sensitive to fee/rounding dust; keep a tiny lamport floor.
            let min_floor = sol_amount_lamports.saturating_add(LOW_SOL_POOL_BUY_BUFFER_LAMPORTS);
            max_sol_cost = max_sol_cost.max(min_floor);
        }

        max_sol_cost
    }

    fn priority_fee_lamports(priority_fee_micro_lamports: u64, compute_units: u32) -> u64 {
        let total_micro_lamports =
            u128::from(priority_fee_micro_lamports).saturating_mul(u128::from(compute_units));
        total_micro_lamports
            .div_ceil(1_000_000)
            .min(u64::MAX as u128) as u64
    }

    fn estimated_buy_balance_lamports(
        max_sol_cost: u64,
        ata_rent_lamports: u64,
        priority_fee_micro_lamports: u64,
    ) -> u64 {
        max_sol_cost
            .saturating_add(ata_rent_lamports)
            .saturating_add(Self::priority_fee_lamports(
                priority_fee_micro_lamports,
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            ))
            .saturating_add(BUY_NETWORK_FEE_BUFFER_LAMPORTS)
    }

    fn lamports_to_sol_ui(lamports: u64) -> f64 {
        lamports as f64 / 1e9
    }

    async fn token_account_rent_lamports(&self, program: &Pubkey) -> anyhow::Result<u64> {
        let space = if *program == TOKEN_PROGRAM_ID {
            <SplTokenAccount as Pack>::LEN
        } else if *program == TOKEN_2022_PROGRAM_ID {
            <SplToken2022Account as Pack>::LEN
        } else {
            anyhow::bail!(
                "unsupported token program for pump.fun buy balance check: {}",
                program
            );
        };

        self.sol
            .rpc_client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .context("failed to fetch token-account rent for pump.fun buy")
    }

    async fn ensure_buy_budget(
        &self,
        buyer: &Pubkey,
        associated_user: &Pubkey,
        program: &Pubkey,
        max_sol_cost: u64,
        priority_fee_micro_lamports: u64,
    ) -> anyhow::Result<()> {
        let ata_rent_lamports = if self.account_exists(associated_user).await? {
            0
        } else {
            self.token_account_rent_lamports(program).await?
        };
        let estimated_network_fee_lamports = Self::priority_fee_lamports(
            priority_fee_micro_lamports,
            DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
        )
        .saturating_add(BUY_NETWORK_FEE_BUFFER_LAMPORTS);
        let required_lamports = Self::estimated_buy_balance_lamports(
            max_sol_cost,
            ata_rent_lamports,
            priority_fee_micro_lamports,
        );
        let balance_lamports = self
            .sol
            .rpc_client
            .get_balance(buyer)
            .await
            .context("failed to fetch buyer SOL balance for pump.fun buy")?;
        let ata_rent_clause = if ata_rent_lamports > 0 {
            format!(
                " + {:.6} SOL token-account rent",
                Self::lamports_to_sol_ui(ata_rent_lamports)
            )
        } else {
            String::new()
        };

        anyhow::ensure!(
            balance_lamports >= required_lamports,
            "insufficient SOL for pump.fun buy: wallet balance is {:.6} SOL but at least {:.6} SOL is required ({:.6} SOL max swap cost{} + ~{:.6} SOL network fees)",
            Self::lamports_to_sol_ui(balance_lamports),
            Self::lamports_to_sol_ui(required_lamports),
            Self::lamports_to_sol_ui(max_sol_cost),
            ata_rent_clause,
            Self::lamports_to_sol_ui(estimated_network_fee_lamports)
        );

        Ok(())
    }

    pub async fn check_socials(&self, uri: &str, retries: u32) -> anyhow::Result<Socials> {
        if !uri.starts_with(PUMP_FUN_URI) && !uri.starts_with(RAPID_LAUNCH_URI) {
            warn!("Socials Check | URI is not a pump.fun URI: {uri}");
            return Ok(Socials {
                ..Default::default()
            });
        }

        let mut retry = 0;

        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json"),
        );

        let mut socials = Socials {
            ..Default::default()
        };

        while retry < retries {
            let res = self.client.get(uri).headers(headers.clone()).send().await?;
            let data = match res.json::<serde_json::Value>().await {
                Ok(data) => data,
                Err(_) => {
                    retry += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            socials = Socials {
                twitter: data
                    .get("twitter")
                    .unwrap_or_default()
                    .as_str()
                    .map(|s| s.to_string()),
                telegram: data
                    .get("telegram")
                    .unwrap_or_default()
                    .as_str()
                    .map(|s| s.to_string()),
                website: data
                    .get("website")
                    .unwrap_or_default()
                    .as_str()
                    .map(|s| s.to_string()),
            };
            break;
        }
        Ok(socials)
    }

    pub async fn buy(
        &self,
        mint: &Pubkey,
        bonding_curve: &Pubkey,
        creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        price: f64,
        use_idempotent: Option<bool>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.buy_with_priority_fee_override(
            mint,
            bonding_curve,
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
        bonding_curve: &Pubkey,
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
            bonding_curve,
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
        bonding_curve: &Pubkey,
        _creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        _price: f64,
        use_idempotent: Option<bool>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.buy_for_user_with_priority_fee_override(
            buyer,
            mint,
            bonding_curve,
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
        bonding_curve: &Pubkey,
        _creator: &Pubkey,
        sol_amount_in: f64,
        slippage: f64,
        _price: f64,
        use_idempotent: Option<bool>,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        let buyer = *buyer;
        let program = self
            .resolve_mint_token_program_with_legacy_fallback(mint, "pump.fun buy")
            .await;
        let slippage_pct = Self::normalize_slippage(slippage);
        let sol_amount_lamports = Self::sol_to_lamports(sol_amount_in);

        let mut ixs = vec![];
        if use_idempotent.unwrap_or(false) {
            ixs.push(create_associated_token_account_idempotent(
                &buyer, &buyer, mint, &program,
            ));
        } else {
            ixs.push(create_associated_token_account(
                &buyer, &buyer, mint, &program,
            ));
        };

        let associated_bc: Pubkey;
        let associated_user: Pubkey;
        if program == TOKEN_PROGRAM_ID {
            associated_bc = self.sol.get_ata_for_token(bonding_curve, mint);
            associated_user = self.sol.get_ata_for_token(&buyer, mint);
        } else {
            associated_bc = self.sol.get_ata_for_token2022(bonding_curve, mint);
            associated_user = self.sol.get_ata_for_token2022(&buyer, mint);
        }
        let curve_state = self
            .fetch_state(bonding_curve)
            .await
            .context("failed to fetch bonding curve state for pump.fun buy")?;
        let global_state = self
            .fetch_global_state()
            .await
            .context("failed to fetch pump.fun global state for buy")?;
        let fee_config = self
            .fetch_fee_config()
            .await
            .context("failed to fetch pump.fun fee config for buy")?;
        let token_amount_out = Self::quote_buy_token_amount_out_raw(
            &global_state,
            fee_config.as_ref(),
            &curve_state,
            sol_amount_lamports,
        )
        .context("failed to quote pump.fun buy output")?;
        anyhow::ensure!(
            token_amount_out > 0,
            "pump.fun buy quote returned zero tokens"
        );
        let max_sol_cost = Self::buy_max_sol_cost(
            sol_amount_lamports,
            slippage_pct,
            curve_state.real_sol_reserves,
        );
        let fee_recipient = Self::fee_recipient_for_mode(&global_state, curve_state.is_mayhem_mode);
        let user_volume_accu = self.user_volume_accu_pda(&buyer);
        let vault = self.creator_vault_pda(&curve_state.creator);
        let bonding_curve_v2 =
            Pubkey::find_program_address(&[b"bonding-curve-v2", mint.as_ref()], &PUMP_FUN_ID).0;
        let buyback_fee_recipient = Self::buyback_fee_recipient(&buyer, mint);

        if !self.sol.exists(&user_volume_accu).await.unwrap_or(false) {
            ixs.push(Instruction {
                program_id: PUMP_FUN_ID,
                accounts: vec![
                    AccountMeta::new(buyer, true),           // payer
                    AccountMeta::new_readonly(buyer, false), // user
                    AccountMeta::new(user_volume_accu, false),
                    AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
                    AccountMeta::new_readonly(EVENT_AUTHORITY, false),
                    AccountMeta::new_readonly(PUMP_FUN_ID, false),
                ],
                data: INIT_USER_VOLUME_ACCUMULATOR_DISCRIM.to_vec(),
            });
        }

        let mut accs = vec![
            AccountMeta::new_readonly(GLOBAL, false),           // global
            AccountMeta::new(fee_recipient, false),             // feeRecipient (writable)
            AccountMeta::new_readonly(*mint, false),            // mint
            AccountMeta::new(*bonding_curve, false),            // bondingCurve (writable)
            AccountMeta::new(associated_bc, false),             // associatedBondingCurve (writable)
            AccountMeta::new(associated_user, false),           // associatedUser (writable)
            AccountMeta::new(buyer, true),                      // user (signer, writable)
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),   // systemProgram
            AccountMeta::new_readonly(program, false),          // tokenProgram
            AccountMeta::new(vault, false),                     // vault (writable)
            AccountMeta::new_readonly(EVENT_AUTHORITY, false),  // eventAuthority
            AccountMeta::new_readonly(PUMP_FUN_ID, false),      // program
            AccountMeta::new(GLOBAL_VOLUME_ACCUMULATOR, false), // globalVolumeAccumulator (writable)
            AccountMeta::new(user_volume_accu, false),          // userVolumeAccumulator (writable)
            AccountMeta::new_readonly(FEE_CONFIG, false),       // feeConfig
            AccountMeta::new_readonly(FEE_PROGRAM, false),      // feeProgram
            // Required by newer Pump SDKs as a trailing remaining account.
            AccountMeta::new_readonly(bonding_curve_v2, false), // bondingCurveV2
        ];
        accs.push(AccountMeta::new(buyback_fee_recipient, false));

        let recent_fees = self
            .sol
            .resolve_priority_fee(
                priority_fee_override,
                &accs.iter().map(|acc| acc.pubkey).collect::<Vec<Pubkey>>(),
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            )
            .await
            .context("failed to resolve priority fee for pump.fun buy")?;
        log!("Fee: {:?}", recent_fees);
        self.ensure_buy_budget(
            &buyer,
            &associated_user,
            &program,
            max_sol_cost,
            recent_fees,
        )
        .await?;
        let mut data = [0u8; 8 + 8 + 8 + 1];
        data[..8].copy_from_slice(&BUY_DISCRIM);
        data[8..16].copy_from_slice(&token_amount_out.to_le_bytes());
        data[16..24].copy_from_slice(&max_sol_cost.to_le_bytes());
        data[24] = 0; // track_volume = false

        ixs.push(Instruction {
            program_id: PUMP_FUN_ID,
            accounts: accs,
            data: data.to_vec(),
        });
        Ok((ixs, recent_fees))
    }

    pub async fn sell(
        &self,
        mint: &Pubkey,
        bonding_curve: &Pubkey,
        creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        price: f64,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.sell_with_priority_fee_override(
            mint,
            bonding_curve,
            creator,
            sell_pct,
            slippage,
            price,
            None,
        )
        .await
    }

    pub async fn sell_with_priority_fee_override(
        &self,
        mint: &Pubkey,
        bonding_curve: &Pubkey,
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
            bonding_curve,
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
        bonding_curve: &Pubkey,
        _creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        _price: f64,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        self.sell_for_user_with_priority_fee_override(
            buyer,
            mint,
            bonding_curve,
            _creator,
            sell_pct,
            slippage,
            _price,
            None,
        )
        .await
    }

    pub async fn sell_for_user_with_priority_fee_override(
        &self,
        buyer: &Pubkey,
        mint: &Pubkey,
        bonding_curve: &Pubkey,
        _creator: &Pubkey,
        sell_pct: u64,
        slippage: f64,
        _price: f64,
        priority_fee_override: Option<PriorityFeeOverride>,
    ) -> anyhow::Result<(Vec<Instruction>, u64)> {
        let buyer = *buyer;
        let program = self
            .resolve_mint_token_program_with_legacy_fallback(mint, "pump.fun sell")
            .await;
        let associated_user = if program == TOKEN_PROGRAM_ID {
            self.sol.get_ata_for_token(&buyer, mint)
        } else {
            self.sol.get_ata_for_token2022(&buyer, mint)
        };

        let (token_balance_raw, _token_decimals) = self
            .sol
            .get_token_balance_raw_from_ata(&associated_user)
            .await
            .context("failed to fetch token balance for pump.fun sell")?;
        let slippage_pct = Self::normalize_slippage(slippage);
        let sell_pct = sell_pct.clamp(1, 100);
        anyhow::ensure!(token_balance_raw > 0, "no token balance for pump.fun sell");

        let token_amount_out = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            token_amount_out > 0,
            "pump.fun sell amount is too small for requested percentage"
        );

        let mut ixs = vec![];
        let associated_bc: Pubkey;
        if program == TOKEN_PROGRAM_ID {
            associated_bc = self.sol.get_ata_for_token(bonding_curve, mint);
        } else {
            associated_bc = self.sol.get_ata_for_token2022(bonding_curve, mint);
        }
        let curve_state = self
            .fetch_state(bonding_curve)
            .await
            .context("failed to fetch bonding curve state for pump.fun sell")?;
        let global_state = self
            .fetch_global_state()
            .await
            .context("failed to fetch pump.fun global state for sell")?;
        let fee_config = self
            .fetch_fee_config()
            .await
            .context("failed to fetch pump.fun fee config for sell")?;
        let quoted_output = Self::quote_sell_sol_output_raw(
            &global_state,
            fee_config.as_ref(),
            &curve_state,
            token_amount_out,
        )
        .context("failed to quote pump.fun sell output")?;
        anyhow::ensure!(
            quoted_output > 0,
            "pump.fun sell quote returned zero SOL output"
        );
        let min_sol_output = Self::apply_sell_slippage(quoted_output, slippage_pct);
        let fee_recipient = Self::fee_recipient_for_mode(&global_state, curve_state.is_mayhem_mode);
        let vault = self.creator_vault_pda(&curve_state.creator);
        let bonding_curve_v2 =
            Pubkey::find_program_address(&[b"bonding-curve-v2", mint.as_ref()], &PUMP_FUN_ID).0;
        let buyback_fee_recipient = Self::buyback_fee_recipient(&buyer, mint);
        let mut accs = vec![
            AccountMeta::new_readonly(GLOBAL, false),          // global
            AccountMeta::new(fee_recipient, false),            // feeRecipient (writable)
            AccountMeta::new_readonly(*mint, false),           // mint
            AccountMeta::new(*bonding_curve, false),           // bondingCurve (writable)
            AccountMeta::new(associated_bc, false),            // associatedBondingCurve (writable)
            AccountMeta::new(associated_user, false),          // associatedUser (writable)
            AccountMeta::new(buyer, true),                     // user (signer, writable)
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),  // systemProgram
            AccountMeta::new(vault, false),                    // vault (writable)
            AccountMeta::new_readonly(program, false),         // tokenProgram
            AccountMeta::new_readonly(EVENT_AUTHORITY, false), // eventAuthority
            AccountMeta::new_readonly(PUMP_FUN_ID, false),     // program
            AccountMeta::new_readonly(FEE_CONFIG, false),      // feeConfig
            AccountMeta::new_readonly(FEE_PROGRAM, false),     // feeProgram
        ];
        if curve_state.is_cashback_coin {
            accs.push(AccountMeta::new(self.user_volume_accu_pda(&buyer), false));
        }
        // Required by newer Pump SDKs as a trailing remaining account.
        accs.push(AccountMeta::new_readonly(bonding_curve_v2, false)); // bondingCurveV2
        accs.push(AccountMeta::new(buyback_fee_recipient, false));

        let recent_fees = self
            .sol
            .resolve_priority_fee(
                priority_fee_override,
                &accs.iter().map(|acc| acc.pubkey).collect::<Vec<Pubkey>>(),
                DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS,
            )
            .await
            .context("failed to resolve priority fee for pump.fun sell")?;
        log!("Fee: {:?}", recent_fees);
        let mut data = [0u8; 8 + 8 + 8];
        data[..8].copy_from_slice(&SELL_DISCRIM);
        data[8..16].copy_from_slice(&token_amount_out.to_le_bytes());
        data[16..24].copy_from_slice(&min_sol_output.to_le_bytes());

        ixs.push(Instruction {
            program_id: PUMP_FUN_ID,
            accounts: accs,
            data: data.to_vec(),
        });

        if sell_pct >= 100 && self.account_exists(&associated_user).await? {
            let close_token_ix =
                self.sol
                    .close_token_account_ix(&program, &associated_user, &buyer, &buyer)?;
            ixs.push(close_token_ix);
        }

        let wsol_program = self
            .sol
            .get_token_program_id(&WSOL_MINT)
            .await
            .context("failed to resolve WSOL token program for pump.fun sell cleanup")?;
        let wsol_ata = if wsol_program == TOKEN_PROGRAM_ID {
            self.sol.get_ata_for_token(&buyer, &WSOL_MINT)
        } else if wsol_program == TOKEN_2022_PROGRAM_ID {
            self.sol.get_ata_for_token2022(&buyer, &WSOL_MINT)
        } else {
            anyhow::bail!(
                "unsupported token program for WSOL cleanup in pump.fun sell: {}",
                wsol_program
            );
        };

        if self.account_exists(&wsol_ata).await? {
            let close_wsol_ix =
                self.sol
                    .close_token_account_ix(&wsol_program, &wsol_ata, &buyer, &buyer)?;
            ixs.push(close_wsol_ix);
        }

        Ok((ixs, recent_fees))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    use borsh010::BorshSerialize as BorshSerializeV0;
    use std::str::FromStr;

    const PUMP_FUN_REAL_TRADE_LOG: &str = "Program data: vdt/007mYe4c+O/+bkHLCAeXOSuIR2NEeyyOkSBVzzT7lqgtUfx5Lwu9/Q8AAAAA91T5xQoCAAABC7E/AxByi1Cv9R5I6i/2UCh/HqByP2RvSXReiyxmaroEE4lpAAAAAAv0nXgOAAAAQAZASArXAQALSHp8BwAAAEBuLfx42AAASsL40N1cvJfjKJwZfLUGKlTz2Va5zm5RFfllZ6pcs+ZfAAAAAAAAAPnjJgAAAAAA0RC6CttaJR83PB1og2nLsCmAMFUqCb0K2CYxyTVXK44eAAAAAAAAAP5HDAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAwAAAGJ1eQA=";

    fn encode_fixture_event<T: BorshSerializeV0>(discriminator: &[u8; 8], event: &T) -> String {
        let mut payload = Vec::with_capacity(512);
        payload.extend_from_slice(discriminator);
        event
            .serialize(&mut payload)
            .expect("fixture event serialization must succeed");
        B64.encode(payload)
    }

    fn test_pump_fun() -> PumpFun {
        let keypair = Arc::new(Keypair::new());
        let sol = Arc::new(SolHook::new("http://127.0.0.1:8899".to_string()));
        PumpFun::new(keypair, sol)
    }

    fn test_global_state(protocol_fee_bps: u64, creator_fee_bps: u64) -> PumpGlobalAccountState {
        PumpGlobalAccountState {
            initialized: true,
            authority: Pubkey::new_unique(),
            fee_recipient: Pubkey::new_unique(),
            initial_virtual_token_reserves: 0,
            initial_virtual_sol_reserves: 0,
            initial_real_token_reserves: 0,
            token_total_supply: ONE_BILLION_SUPPLY_RAW as u64,
            fee_basis_points: protocol_fee_bps,
            withdraw_authority: Pubkey::new_unique(),
            enable_migrate: false,
            pool_migration_fee: 0,
            creator_fee_basis_points: creator_fee_bps,
            fee_recipients: [Pubkey::default(); 7],
            set_creator_authority: Pubkey::new_unique(),
            admin_set_creator_authority: Pubkey::new_unique(),
            create_v2_enabled: true,
            whitelist_pda: Pubkey::new_unique(),
            reserved_fee_recipient: Pubkey::new_unique(),
            mayhem_mode_enabled: false,
            reserved_fee_recipients: [Pubkey::default(); 7],
            is_cashback_enabled: true,
        }
    }

    #[test]
    fn test_pump_fun_discriminators_match_idl() {
        assert_eq!(CREATE_DISCRIM, [27, 114, 169, 77, 222, 235, 99, 118]);
        assert_eq!(TRADE_DISCRIM, [189, 219, 127, 211, 78, 230, 97, 238]);
        assert_eq!(BUY_DISCRIM, [102, 6, 61, 18, 1, 218, 235, 234]);
        assert_eq!(
            BUY_EXACT_SOL_IN_DISCRIM,
            [56, 252, 116, 8, 158, 223, 205, 95]
        );
        assert_eq!(SELL_DISCRIM, [51, 230, 133, 164, 1, 127, 131, 173]);
    }

    #[test]
    fn test_pump_fun_program_constants() {
        let expected_program =
            Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P").unwrap();
        let expected_global =
            Pubkey::from_str("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf").unwrap();
        assert_eq!(PUMP_FUN_ID, expected_program);
        assert_eq!(GLOBAL, expected_global);
    }

    #[tokio::test]
    async fn test_pump_fun_derive_bonding_curve_is_stable() {
        let mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        let (expected, _) =
            Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &PUMP_FUN_ID);
        let derived = PumpFun::derive_bonding_curve(&mint).await.unwrap();
        assert_eq!(derived, expected);
    }

    #[test]
    fn test_pump_fun_lamports_to_tokens_conversion() {
        let pump_fun = test_pump_fun();

        // 1 SOL at 0.001 SOL/token should result in 1000 tokens => 1_000_000_000 raw units (6 decimals).
        let out = pump_fun.lamports_to_tokens(1_000_000_000.0, 0.001);
        assert_eq!(out, 1_000_000_000);
    }

    #[test]
    fn test_pump_fun_normalize_slippage() {
        assert!((PumpFun::normalize_slippage(15.0) - 0.15).abs() < 1e-12);
        assert!((PumpFun::normalize_slippage(0.15) - 0.15).abs() < 1e-12);
        assert!((PumpFun::normalize_slippage(150.0) - 0.99).abs() < 1e-12);
        assert!((PumpFun::normalize_slippage(-1.0) - 0.01).abs() < 1e-12);
    }

    #[test]
    fn test_pump_fun_buy_max_sol_cost_applies_low_sol_pool_buffer() {
        let max_cost = PumpFun::buy_max_sol_cost(1_001, 0.01, 1_941_841);
        assert_eq!(max_cost, 1_017);
    }

    #[test]
    fn test_pump_fun_buy_max_sol_cost_healthy_pool_uses_slippage_only() {
        let max_cost = PumpFun::buy_max_sol_cost(1_001, 0.01, 20_000_000);
        assert_eq!(max_cost, 1_012);
    }

    #[test]
    fn test_pump_fun_priority_fee_lamports_uses_compute_units() {
        assert_eq!(
            PumpFun::priority_fee_lamports(1_000_000, DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS),
            300_000
        );
    }

    #[test]
    fn test_pump_fun_estimated_buy_balance_includes_rent_and_fee_buffer() {
        let required = PumpFun::estimated_buy_balance_lamports(10_000, 2_039_280, 1_000_000);
        assert_eq!(
            required,
            10_000 + 2_039_280 + 300_000 + BUY_NETWORK_FEE_BUFFER_LAMPORTS
        );
    }

    #[test]
    fn test_pump_fun_decode_bonding_curve_account_reads_flags() {
        let raw = BondingCurveAccountRaw {
            virtual_token_reserves: 1,
            virtual_sol_reserves: 2,
            real_token_reserves: 3,
            real_sol_reserves: 4,
            token_total_supply: 5,
            complete: false,
            creator: Pubkey::new_unique(),
        };

        let mut serialized = vec![];
        borsh::BorshSerialize::serialize(&raw, &mut serialized)
            .expect("serializing fixture bonding curve must succeed");

        let mut account_data = vec![0u8; 8];
        account_data.extend_from_slice(&serialized);
        account_data.push(1); // trailing mayhem-mode byte
        account_data.push(1); // trailing cashback byte

        let decoded =
            PumpFun::decode_bonding_curve_account_data(&account_data).expect("decode must succeed");
        assert!(decoded.is_mayhem_mode);
        assert!(decoded.is_cashback_coin);
    }

    #[test]
    fn test_pump_fun_fee_recipient_switches_for_mayhem_mode() {
        let global = test_global_state(100, 50);
        assert_eq!(
            PumpFun::fee_recipient_for_mode(&global, false),
            global.fee_recipient
        );
        assert_eq!(
            PumpFun::fee_recipient_for_mode(&global, true),
            global.reserved_fee_recipient
        );
    }

    #[test]
    fn test_pump_fun_buyback_fee_recipient_is_stable() {
        let user = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let recipient = PumpFun::buyback_fee_recipient(&user, &mint);
        assert!(BUYBACK_FEE_RECIPIENTS.contains(&recipient));
        assert_eq!(recipient, PumpFun::buyback_fee_recipient(&user, &mint));
    }

    #[test]
    fn test_pump_fun_quote_buy_token_amount_out_matches_official_fee_model() {
        let global = test_global_state(100, 50);
        let curve = BondingCurveAccount {
            virtual_token_reserves: 1_000_000_000,
            virtual_sol_reserves: 2_500_000_000,
            real_token_reserves: 1_000_000_000,
            real_sol_reserves: 0,
            token_total_supply: ONE_BILLION_SUPPLY_RAW as u64,
            complete: false,
            creator: Pubkey::new_unique(),
            is_mayhem_mode: false,
            is_cashback_coin: false,
        };

        let quoted =
            PumpFun::quote_buy_token_amount_out_raw(&global, None, &curve, 10_000_000).unwrap();
        assert_eq!(quoted, 3_925_416);
    }

    #[test]
    fn test_pump_fun_quote_sell_sol_output_matches_official_fee_model() {
        let global = test_global_state(100, 50);
        let curve = BondingCurveAccount {
            virtual_token_reserves: 1_000_000_000,
            virtual_sol_reserves: 2_500_000_000,
            real_token_reserves: 1_000_000_000,
            real_sol_reserves: 0,
            token_total_supply: ONE_BILLION_SUPPLY_RAW as u64,
            complete: false,
            creator: Pubkey::new_unique(),
            is_mayhem_mode: false,
            is_cashback_coin: false,
        };

        let quoted =
            PumpFun::quote_sell_sol_output_raw(&global, None, &curve, 100_000_000).unwrap();
        assert_eq!(quoted, 223_863_635);
        assert_eq!(PumpFun::apply_sell_slippage(quoted, 0.15), 190_284_089);
    }

    #[test]
    fn test_pump_fun_parse_logs_ignores_invalid_and_short_payloads() {
        let logs = vec![
            "Program log: hello".to_string(),
            "Program data: not-base64".to_string(),
            format!("Program data: {}", B64.encode([1u8, 2, 3, 4])), // too short (< 8)
        ];

        let events = PumpFun::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_pump_fun_parse_logs_unknown_discriminator_is_ignored() {
        // 8-byte discriminator that does not match any known PumpFun event.
        let payload = [9u8, 9, 9, 9, 9, 9, 9, 9, 1, 2, 3];
        let logs = vec![format!("Program data: {}", B64.encode(payload))];

        let events = PumpFun::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_pump_fun_parse_logs_real_trade_fixture() {
        let logs = vec![PUMP_FUN_REAL_TRADE_LOG.to_string()];
        let events = PumpFun::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);

        match &events[0] {
            PumpFunEvent::Trade(Some(trade)) => {
                assert!(trade.timestamp > 0);
                assert!(trade.virtual_sol_reserves > 0);
                assert!(trade.virtual_token_reserves > 0);
                assert!(trade.real_sol_reserves > 0);
                assert!(trade.token_amount > 0);
            }
            _ => panic!("expected parsed PumpFun trade event"),
        }
    }

    #[test]
    fn test_pump_fun_parse_logs_create_fixture_decodes() {
        let fixture = CreateEvent {
            name: "FixtureToken".to_string(),
            symbol: "FIX".to_string(),
            uri: "https://example.com/token.json".to_string(),
            timestamp: 1_735_000_000,
            virtual_token_reserves: 1_000_000_000,
            virtual_sol_reserves: 2_000_000_000,
            real_token_reserves: 800_000_000,
            token_total_supply: 1_000_000_000_000,
            ..Default::default()
        };

        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&CREATE_DISCRIM, &fixture)
        )];

        let events = PumpFun::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);

        match &events[0] {
            PumpFunEvent::Create(Some(event)) => {
                assert_eq!(event.name, "FixtureToken");
                assert_eq!(event.symbol, "FIX");
                assert_eq!(event.virtual_token_reserves, fixture.virtual_token_reserves);
                assert_eq!(event.virtual_sol_reserves, fixture.virtual_sol_reserves);
                assert_eq!(event.token_total_supply, fixture.token_total_supply);
            }
            _ => panic!("expected parsed PumpFun create event"),
        }
    }

    #[test]
    fn test_pump_fun_get_price_and_market_cap_from_trade_event() {
        let trade = TradeEvent {
            mint: Default::default(),
            sol_amount: 0,
            token_amount: 0,
            is_buy: true,
            user: Default::default(),
            timestamp: 0,
            virtual_sol_reserves: 2_500_000_000,   // 2.5 SOL
            virtual_token_reserves: 1_000_000_000, // 1000 tokens (6 decimals)
            real_sol_reserves: 0,
            real_token_reserves: 0,
            fee_recipient: Default::default(),
            fee_basis_points: 0,
            fee: 0,
            creator: Default::default(),
            creator_fee_basis_points: 0,
            creator_fee: 0,
            track_volume: false,
            total_unclaimed_tokens: 0,
            total_claimed_tokens: 0,
            current_sol_volume: 0,
            last_update_timestamp: 0,
        };

        let price = PumpFun::get_price(&trade);
        assert!((price - 0.0025).abs() < 1e-12);

        let mc = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(PumpFun::get_market_cap(&trade, 200.0))
            .unwrap();
        assert!((mc - 500_000_000.0).abs() < 1e-3);
    }
}
