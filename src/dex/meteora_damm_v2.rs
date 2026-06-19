use crate::core::cluster::{DEFAULT_DEVNET_HTTP_URL, DEFAULT_MAINNET_HTTP_URL, SolanaCluster};
use crate::core::sol::{
    DEFAULT_PRIORITY_FEE_CLAMP_COMPUTE_UNITS, PriorityFeeOverride, SolHook, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, WSOL_MINT,
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

pub const METEORA_DAMM_V2_ID: Pubkey =
    Pubkey::from_str_const("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
pub const METEORA_DAMM_V2_POOL_AUTHORITY: Pubkey =
    Pubkey::from_str_const("HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC");

// anchor discriminators
pub const CONFIG_DISCRIM: [u8; 8] = [155, 12, 170, 224, 30, 250, 204, 130]; // account:Config
pub const INITIALIZE_POOL_IX_DISCRIM: [u8; 8] = [95, 180, 10, 172, 84, 174, 232, 40]; // global:initialize_pool

pub const SWAP_IX_DISCRIM: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
pub const POOL_DISCRIM: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
pub const EVT_INITIALIZE_POOL_EVENT_DISCRIM: [u8; 8] = [228, 50, 246, 85, 203, 66, 134, 37];
pub const EVT_SWAP2_EVENT_DISCRIM: [u8; 8] = [189, 66, 51, 168, 38, 80, 117, 153];

pub const SEARCH_FOR: &str = "Program data: ";
pub const POOL_AUTHORITY_SEED: &[u8] = b"pool_authority";
pub const EVENT_AUTHORITY_SEED: &[u8] = b"__event_authority";

pub const POOL_PREFIX: &[u8] = b"pool";
pub const TOKEN_VAULT_PREFIX: &[u8] = b"token_vault";
pub const POSITION_PREFIX: &[u8] = b"position";
pub const POSITION_NFT_ACCOUNT_PREFIX: &[u8] = b"position_nft_account";
pub const CUSTOMIZABLE_POOL_PREFIX: &[u8] = b"cpool";
pub const TOKEN_BADGE_PREFIX: &[u8] = b"token_badge";

pub const DAMM_V2_POOL_MIN_DECODE_LEN: usize = 488;
pub const DAMM_V2_POOL_ACCOUNT_LEN: usize = 8 + 1104;
pub const DAMM_V2_POOL_TOKEN_A_MINT_OFFSET: usize = 168;
pub const DAMM_V2_POOL_TOKEN_B_MINT_OFFSET: usize = 200;
pub const DAMM_V2_POOL_A_VAULT_OFFSET: usize = 232;
pub const DAMM_V2_POOL_B_VAULT_OFFSET: usize = 264;
pub const DAMM_V2_POOL_WHITELISTED_VAULT_OFFSET: usize = 296;
pub const DAMM_V2_POOL_PARTNER_OFFSET: usize = 328;
pub const DAMM_V2_POOL_LIQUIDITY_OFFSET: usize = 360;
pub const DAMM_V2_POOL_PROTOCOL_A_FEE_OFFSET: usize = 392;
pub const DAMM_V2_POOL_PROTOCOL_B_FEE_OFFSET: usize = 400;
pub const DAMM_V2_POOL_PARTNER_A_FEE_OFFSET: usize = 408;
pub const DAMM_V2_POOL_PARTNER_B_FEE_OFFSET: usize = 416;
pub const DAMM_V2_POOL_SQRT_MIN_PRICE_OFFSET: usize = 424;
pub const DAMM_V2_POOL_SQRT_MAX_PRICE_OFFSET: usize = 440;
pub const DAMM_V2_POOL_SQRT_PRICE_OFFSET: usize = 456;
pub const DAMM_V2_POOL_ACTIVATION_POINT_OFFSET: usize = 472;
pub const DAMM_V2_POOL_ACTIVATION_TYPE_OFFSET: usize = 480;
pub const DAMM_V2_POOL_STATUS_OFFSET: usize = 481;
pub const DAMM_V2_POOL_TOKEN_A_FLAG_OFFSET: usize = 482;
pub const DAMM_V2_POOL_TOKEN_B_FLAG_OFFSET: usize = 483;
pub const DAMM_V2_POOL_COLLECT_FEE_MODE_OFFSET: usize = 484;
pub const DAMM_V2_POOL_TYPE_OFFSET: usize = 485;
pub const DAMM_V2_POOL_VERSION_OFFSET: usize = 486;
pub const DAMM_V2_POOL_CREATOR_OFFSET: usize = 648;
pub const DAMM_V2_POOL_TOKEN_A_AMOUNT_OFFSET: usize = 680;
pub const DAMM_V2_POOL_TOKEN_B_AMOUNT_OFFSET: usize = 688;
pub const DAMM_V2_POOL_LAYOUT_VERSION_OFFSET: usize = 696;

const POOL_STATUS_ENABLED: u8 = 0;

#[derive(Debug, Clone)]
pub struct DammV2PoolState {
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
    pub whitelisted_vault: Pubkey,
    pub partner: Pubkey,
    pub liquidity: u128,
    pub protocol_a_fee: u64,
    pub protocol_b_fee: u64,
    pub partner_a_fee: u64,
    pub partner_b_fee: u64,
    pub sqrt_min_price: u128,
    pub sqrt_max_price: u128,
    pub sqrt_price: u128,
    pub activation_point: u64,
    pub activation_type: u8,
    pub pool_status: u8,
    pub token_a_flag: u8,
    pub token_b_flag: u8,
    pub collect_fee_mode: u8,
    pub pool_type: u8,
    pub version: u8,
    pub creator: Pubkey,
    pub token_a_amount: u64,
    pub token_b_amount: u64,
    pub layout_version: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InitializePoolEvent {
    pub pool: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub creator: Pubkey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Swap2Event {
    pub pool: Pubkey,
    pub trade_direction: u8,
    pub collect_fee_mode: u8,
    pub has_referral: bool,
    pub amount_0: u64,
    pub amount_1: u64,
    pub swap_mode: u8,
    pub included_fee_input_amount: u64,
    pub excluded_fee_input_amount: u64,
    pub output_amount: u64,
    pub reserve_a_amount: u64,
    pub reserve_b_amount: u64,
}

#[derive(Debug)]
pub enum MeteoraDammV2Event {
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
            token_a_mint: read_pubkey_cursor(cur)?,
            token_b_mint: read_pubkey_cursor(cur)?,
            creator: read_pubkey_cursor(cur)?,
        })
    }

    #[cfg(test)]
    fn to_bytes_with_padding(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.pool.as_ref());
        out.extend_from_slice(self.token_a_mint.as_ref());
        out.extend_from_slice(self.token_b_mint.as_ref());
        out.extend_from_slice(self.creator.as_ref());
        out.extend_from_slice(Pubkey::new_unique().as_ref()); // payer
        out.extend_from_slice(Pubkey::new_unique().as_ref()); // alpha_vault
        out.extend_from_slice(&[0u8; 160]); // pool_fees + remaining fields padding for parser
        out
    }
}

impl Swap2Event {
    fn deserialize_from_cursor(cur: &mut Cursor<&[u8]>) -> anyhow::Result<Self> {
        let pool = read_pubkey_cursor(cur)?;
        let trade_direction = read_exact::<1>(cur)?[0];
        let collect_fee_mode = read_exact::<1>(cur)?[0];
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
        let _partner_fee = read_u64_cursor(cur)?;
        let _referral_fee = read_u64_cursor(cur)?;

        let _included_transfer_fee_amount_in = read_u64_cursor(cur)?;
        let _included_transfer_fee_amount_out = read_u64_cursor(cur)?;
        let _excluded_transfer_fee_amount_out = read_u64_cursor(cur)?;
        let _current_timestamp = read_u64_cursor(cur)?;
        let reserve_a_amount = read_u64_cursor(cur)?;
        let reserve_b_amount = read_u64_cursor(cur)?;

        Ok(Self {
            pool,
            trade_direction,
            collect_fee_mode,
            has_referral,
            amount_0,
            amount_1,
            swap_mode,
            included_fee_input_amount,
            excluded_fee_input_amount,
            output_amount,
            reserve_a_amount,
            reserve_b_amount,
        })
    }

    #[cfg(test)]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.pool.as_ref());
        out.push(self.trade_direction);
        out.push(self.collect_fee_mode);
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
        out.extend_from_slice(&1u64.to_le_bytes()); // partner_fee
        out.extend_from_slice(&1u64.to_le_bytes()); // referral_fee

        out.extend_from_slice(&10u64.to_le_bytes());
        out.extend_from_slice(&20u64.to_le_bytes());
        out.extend_from_slice(&15u64.to_le_bytes());
        out.extend_from_slice(&1_700_000_000u64.to_le_bytes());
        out.extend_from_slice(&self.reserve_a_amount.to_le_bytes());
        out.extend_from_slice(&self.reserve_b_amount.to_le_bytes());
        out
    }
}

#[derive(Clone)]
pub struct MeteoraDammV2 {
    pub keypair: Arc<Keypair>,
    pub sol: Arc<SolHook>,
}

impl MeteoraDammV2 {
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

    fn buy_use_idempotent(use_idempotent: Option<bool>) -> bool {
        use_idempotent.unwrap_or(true)
    }

    fn pool_account_filters_for_mint(offset: usize, mint: &Pubkey) -> Vec<RpcFilterType> {
        vec![
            RpcFilterType::DataSize(DAMM_V2_POOL_ACCOUNT_LEN as u64),
            RpcFilterType::Memcmp(Memcmp::new_base58_encoded(offset, mint.as_ref())),
        ]
    }

    fn is_helius_rpc_url(rpc_url: &str) -> bool {
        rpc_url
            .split('?')
            .next()
            .is_some_and(|base| base.contains("helius-rpc.com"))
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

    async fn find_pool_pubkeys_by_mint_with_rpc_client(
        rpc_client: &RpcClient,
        offset: usize,
        mint: &Pubkey,
    ) -> anyhow::Result<Vec<Pubkey>> {
        let cfg = RpcProgramAccountsConfig {
            filters: Some(Self::pool_account_filters_for_mint(offset, mint)),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            with_context: None,
            sort_results: None,
        };
        Ok(rpc_client
            .get_program_ui_accounts_with_config(&METEORA_DAMM_V2_ID, cfg)
            .await?
            .into_iter()
            .map(|(pool, _)| pool)
            .collect())
    }

    async fn find_pool_pubkeys_by_mint_standard_rpc(
        &self,
        offset: usize,
        mint: &Pubkey,
    ) -> anyhow::Result<Vec<Pubkey>> {
        Self::find_pool_pubkeys_by_mint_with_rpc_client(self.sol.rpc_client.as_ref(), offset, mint)
            .await
    }

    async fn find_pool_pubkeys_by_mint_public_fallback(
        &self,
        offset: usize,
        mint: &Pubkey,
    ) -> anyhow::Result<Vec<Pubkey>> {
        let rpc_url = self
            .readonly_fallback_rpc_url()
            .context("no readonly fallback rpc configured for this cluster")?;
        let rpc_client =
            RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed());
        Self::find_pool_pubkeys_by_mint_with_rpc_client(&rpc_client, offset, mint)
            .await
            .with_context(|| {
                format!(
                    "readonly fallback getProgramAccounts failed via {}",
                    rpc_url
                )
            })
    }

    async fn find_pool_pubkeys_by_mint_paginated_helius(
        &self,
        offset: usize,
        mint: &Pubkey,
    ) -> anyhow::Result<Vec<Pubkey>> {
        use reqwest::Client;
        use serde::Deserialize;
        use serde_json::json;

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
        let mut pagination_key = None::<String>;
        let mut pools = BTreeSet::new();
        let rpc_url = self.sol.rpc_client.url().to_string();

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
                        "dataSize": DAMM_V2_POOL_ACCOUNT_LEN
                    },
                    {
                        "memcmp": {
                            "offset": offset,
                            "bytes": mint.to_string()
                        }
                    }
                ],
                "limit": HELIUS_PROGRAM_ACCOUNTS_PAGE_LIMIT
            });
            if let Some(cursor) = pagination_key.as_ref() {
                options["paginationKey"] = json!(cursor);
            }

            let response = client
                .post(&rpc_url)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": "meteora-damm-v2-pool-discovery",
                    "method": "getProgramAccountsV2",
                    "params": [METEORA_DAMM_V2_ID.to_string(), options]
                }))
                .send()
                .await
                .context("helius getProgramAccountsV2 request failed")?;
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
                    account.pubkey.parse::<Pubkey>().with_context(|| {
                        format!("invalid DAMM v2 pool pubkey {}", account.pubkey)
                    })?,
                );
            }

            if result.accounts.is_empty() || result.pagination_key.is_none() {
                break;
            }
            pagination_key = result.pagination_key;
        }

        Ok(pools.into_iter().collect())
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

    pub fn decode_pool_account_data(data: &[u8]) -> anyhow::Result<DammV2PoolState> {
        anyhow::ensure!(
            data.len() >= DAMM_V2_POOL_MIN_DECODE_LEN,
            "meteora damm v2 pool account too short: {}",
            data.len()
        );
        anyhow::ensure!(
            data[..8] == POOL_DISCRIM,
            "meteora damm v2 pool discriminator mismatch"
        );

        Ok(DammV2PoolState {
            token_a_mint: Self::read_pubkey(data, DAMM_V2_POOL_TOKEN_A_MINT_OFFSET)?,
            token_b_mint: Self::read_pubkey(data, DAMM_V2_POOL_TOKEN_B_MINT_OFFSET)?,
            token_a_vault: Self::read_pubkey(data, DAMM_V2_POOL_A_VAULT_OFFSET)?,
            token_b_vault: Self::read_pubkey(data, DAMM_V2_POOL_B_VAULT_OFFSET)?,
            whitelisted_vault: Self::read_pubkey(data, DAMM_V2_POOL_WHITELISTED_VAULT_OFFSET)?,
            partner: Self::read_pubkey(data, DAMM_V2_POOL_PARTNER_OFFSET)?,
            liquidity: Self::read_u128(data, DAMM_V2_POOL_LIQUIDITY_OFFSET)?,
            protocol_a_fee: Self::read_u64(data, DAMM_V2_POOL_PROTOCOL_A_FEE_OFFSET)?,
            protocol_b_fee: Self::read_u64(data, DAMM_V2_POOL_PROTOCOL_B_FEE_OFFSET)?,
            partner_a_fee: Self::read_u64(data, DAMM_V2_POOL_PARTNER_A_FEE_OFFSET)?,
            partner_b_fee: Self::read_u64(data, DAMM_V2_POOL_PARTNER_B_FEE_OFFSET)?,
            sqrt_min_price: Self::read_u128(data, DAMM_V2_POOL_SQRT_MIN_PRICE_OFFSET)?,
            sqrt_max_price: Self::read_u128(data, DAMM_V2_POOL_SQRT_MAX_PRICE_OFFSET)?,
            sqrt_price: Self::read_u128(data, DAMM_V2_POOL_SQRT_PRICE_OFFSET)?,
            activation_point: Self::read_u64(data, DAMM_V2_POOL_ACTIVATION_POINT_OFFSET)?,
            activation_type: data[DAMM_V2_POOL_ACTIVATION_TYPE_OFFSET],
            pool_status: data[DAMM_V2_POOL_STATUS_OFFSET],
            token_a_flag: data[DAMM_V2_POOL_TOKEN_A_FLAG_OFFSET],
            token_b_flag: data[DAMM_V2_POOL_TOKEN_B_FLAG_OFFSET],
            collect_fee_mode: data[DAMM_V2_POOL_COLLECT_FEE_MODE_OFFSET],
            pool_type: data[DAMM_V2_POOL_TYPE_OFFSET],
            version: data[DAMM_V2_POOL_VERSION_OFFSET],
            creator: Self::read_pubkey(data, DAMM_V2_POOL_CREATOR_OFFSET)?,
            token_a_amount: Self::read_u64(data, DAMM_V2_POOL_TOKEN_A_AMOUNT_OFFSET)?,
            token_b_amount: Self::read_u64(data, DAMM_V2_POOL_TOKEN_B_AMOUNT_OFFSET)?,
            layout_version: data[DAMM_V2_POOL_LAYOUT_VERSION_OFFSET],
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

    fn tracked_reserve_raw(state: &DammV2PoolState) -> Option<(u64, u64)> {
        (state.layout_version != 0 && (state.token_a_amount > 0 || state.token_b_amount > 0))
            .then_some((state.token_a_amount, state.token_b_amount))
    }

    fn derive_pool_authority_pda() -> Pubkey {
        Pubkey::find_program_address(&[POOL_AUTHORITY_SEED], &METEORA_DAMM_V2_ID).0
    }

    fn derive_event_authority_pda() -> Pubkey {
        Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], &METEORA_DAMM_V2_ID).0
    }

    fn pool_non_wsol_mint(state: &DammV2PoolState) -> Option<Pubkey> {
        if state.token_a_mint == WSOL_MINT && state.token_b_mint != WSOL_MINT {
            Some(state.token_b_mint)
        } else if state.token_b_mint == WSOL_MINT && state.token_a_mint != WSOL_MINT {
            Some(state.token_a_mint)
        } else {
            None
        }
    }

    fn validate_pool_contains_mint(state: &DammV2PoolState, mint: &Pubkey) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.token_a_mint == *mint || state.token_b_mint == *mint,
            "pool does not contain target mint"
        );
        anyhow::ensure!(
            state.token_a_mint == WSOL_MINT || state.token_b_mint == WSOL_MINT,
            "meteora damm v2 pool is not WSOL-quoted"
        );
        Ok(())
    }

    pub fn price_from_sqrt_price_x64(
        state: &DammV2PoolState,
        token_decimals: u8,
    ) -> anyhow::Result<f64> {
        anyhow::ensure!(state.sqrt_price > 0, "invalid meteora damm v2 sqrt_price");

        let sqrt_ratio = state.sqrt_price as f64 / (1u128 << 64) as f64;
        let price_raw = sqrt_ratio * sqrt_ratio;
        anyhow::ensure!(
            price_raw.is_finite() && price_raw > 0.0,
            "invalid meteora damm v2 raw price"
        );

        let price_sol_per_token_raw = if state.token_b_mint == WSOL_MINT {
            price_raw
        } else if state.token_a_mint == WSOL_MINT {
            1.0 / price_raw
        } else {
            anyhow::bail!("meteora damm v2 pool is not WSOL-quoted");
        };

        let decimal_adjustment = 10_f64.powi(token_decimals as i32 - 9);
        let price = price_sol_per_token_raw * decimal_adjustment;
        anyhow::ensure!(
            price.is_finite() && price > 0.0,
            "invalid meteora damm v2 token price"
        );

        Ok(price)
    }

    async fn fetch_reserve_raw(&self, state: &DammV2PoolState) -> anyhow::Result<(u64, u64)> {
        let a = self
            .sol
            .get_token_balance_raw_from_ata(&state.token_a_vault)
            .await?
            .0;
        let b = self
            .sol
            .get_token_balance_raw_from_ata(&state.token_b_vault)
            .await?
            .0;
        Ok((a, b))
    }

    async fn user_token_balance_raw(&self, owner: &Pubkey, mint: &Pubkey) -> anyhow::Result<u64> {
        let token_program = self
            .sol
            .get_token_program_id(mint)
            .await
            .with_context(|| format!("failed to resolve token program for mint {}", mint))?;
        let ata = Self::ata_for(owner, mint, &token_program);
        let amount = self.sol.get_token_balance_raw_from_ata(&ata).await?.0;
        Ok(amount)
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

    pub fn parse_logs<'a>(
        logs: impl Iterator<Item = &'a String>,
        sig: Option<&String>,
    ) -> Vec<MeteoraDammV2Event> {
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
                    Ok(event) => events.push(MeteoraDammV2Event::InitializePool(Some(event))),
                    Err(e) => warn!(
                        "Error deserializing meteora damm v2 initialize-pool event {:?}: {e}",
                        sig.unwrap_or(&"".to_string())
                    ),
                }
            } else if b64[..8] == EVT_SWAP2_EVENT_DISCRIM {
                let mut cursor = Cursor::new(&b64[8..]);
                match Swap2Event::deserialize_from_cursor(&mut cursor) {
                    Ok(event) => events.push(MeteoraDammV2Event::Swap2(Some(event))),
                    Err(e) => warn!(
                        "Error deserializing meteora damm v2 swap2 event {:?}: {e}",
                        sig.unwrap_or(&"".to_string())
                    ),
                }
            } else {
                events.push(MeteoraDammV2Event::Unknown);
            }
        }
        events
    }

    pub fn extract_pool_from_transaction(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> Option<Pubkey> {
        if let Some(meta) = tx.transaction.meta.as_ref()
            && let OptionSerializer::Some(logs) = &meta.log_messages
        {
            for event in Self::parse_logs(logs.iter(), None) {
                match event {
                    MeteoraDammV2Event::InitializePool(Some(init)) => return Some(init.pool),
                    MeteoraDammV2Event::Swap2(Some(swap)) => return Some(swap.pool),
                    _ => {}
                }
            }
        }

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
                    let pool_idx = *compiled.accounts.get(1)? as usize;
                    let account = account_keys.get(pool_idx)?;
                    Pubkey::from_str(account).ok()
                }
            }
        }

        let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
            return None;
        };
        let program_id = METEORA_DAMM_V2_ID.to_string();

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
                        .get(2)
                        .and_then(|v| Pubkey::from_str(v).ok())
                }
                UiInstruction::Compiled(compiled) => {
                    let program_index = compiled.program_id_index as usize;
                    let program = account_keys.get(program_index)?;
                    if *program != program_id {
                        return None;
                    }
                    let src_idx = *compiled.accounts.get(2)? as usize;
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
        let program_id = METEORA_DAMM_V2_ID.to_string();

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

    pub async fn fetch_state(&self, pool: &Pubkey) -> anyhow::Result<DammV2PoolState> {
        match Self::fetch_state_with_rpc_client(self.sol.rpc_client.as_ref(), pool).await {
            Ok(state) => Ok(state),
            Err(primary_error) => {
                let Some(rpc_url) = self.readonly_fallback_rpc_url() else {
                    return Err(primary_error);
                };
                warn!(
                    "meteora damm v2 fetch_state primary rpc failed for pool {}: {}; retrying via {}",
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

    async fn fetch_states_for_pools(
        &self,
        pools: &[Pubkey],
    ) -> anyhow::Result<Vec<(Pubkey, DammV2PoolState)>> {
        if pools.is_empty() {
            return Ok(Vec::new());
        }

        let accounts = match Self::fetch_pool_accounts_with_rpc_client(
            self.sol.rpc_client.as_ref(),
            pools,
        )
        .await
        {
            Ok(accounts) => accounts,
            Err(primary_error) => {
                let Some(rpc_url) = self.readonly_fallback_rpc_url() else {
                    return Err(primary_error);
                };
                warn!(
                    "meteora damm v2 bulk state fetch failed for {} pools: {}; retrying via {}",
                    pools.len(),
                    primary_error,
                    rpc_url
                );
                let rpc_client = RpcClient::new_with_commitment(
                    rpc_url.to_string(),
                    CommitmentConfig::confirmed(),
                );
                Self::fetch_pool_accounts_with_rpc_client(&rpc_client, pools)
                        .await
                        .with_context(|| {
                            format!(
                                "readonly fallback bulk state fetch failed via {} after primary error: {}",
                                rpc_url, primary_error
                            )
                        })?
            }
        };

        let mut out = Vec::new();
        for (pool, account) in pools.iter().copied().zip(accounts.into_iter()) {
            let Some(account) = account else {
                continue;
            };
            if let Ok(state) = Self::decode_pool_account_data(&account.data) {
                out.push((pool, state));
            }
        }
        Ok(out)
    }

    async fn fetch_state_with_rpc_client(
        rpc_client: &RpcClient,
        pool: &Pubkey,
    ) -> anyhow::Result<DammV2PoolState> {
        let data = rpc_client
            .get_account_with_commitment(pool, CommitmentConfig::processed())
            .await?
            .value
            .ok_or(anyhow::anyhow!("meteora damm v2 pool account not found"))?
            .data;
        Self::decode_pool_account_data(&data)
    }

    async fn fetch_pool_accounts_with_rpc_client(
        rpc_client: &RpcClient,
        pools: &[Pubkey],
    ) -> anyhow::Result<Vec<Option<solana_account::Account>>> {
        Ok(rpc_client
            .get_multiple_accounts_with_commitment(pools, CommitmentConfig::confirmed())
            .await?
            .value)
    }

    pub async fn fetch_wsol_liquidity_raw(&self, state: &DammV2PoolState) -> anyhow::Result<u64> {
        let (reserve_a, reserve_b) =
            if let Some((reserve_a, reserve_b)) = Self::tracked_reserve_raw(state) {
                (reserve_a, reserve_b)
            } else {
                self.fetch_reserve_raw(state).await?
            };
        if state.token_a_mint == WSOL_MINT {
            Ok(reserve_a)
        } else if state.token_b_mint == WSOL_MINT {
            Ok(reserve_b)
        } else {
            anyhow::bail!("meteora damm v2 pool has no WSOL side")
        }
    }

    pub async fn fetch_price(&self, pool: &Pubkey) -> anyhow::Result<(DammV2PoolState, f64)> {
        let state = self.fetch_state(pool).await?;
        let token_mint = Self::pool_non_wsol_mint(&state)
            .ok_or(anyhow::anyhow!("meteora damm v2 pool is not WSOL-quoted"))?;
        let token_decimals = self
            .sol
            .get_token_decimals(&token_mint)
            .await
            .with_context(|| format!("failed to fetch token decimals for mint {}", token_mint))?;

        let price = match Self::price_from_sqrt_price_x64(&state, token_decimals) {
            Ok(price) => price,
            Err(_) => {
                let (reserve_a_raw, reserve_b_raw) =
                    if let Some((reserve_a, reserve_b)) = Self::tracked_reserve_raw(&state) {
                        (reserve_a, reserve_b)
                    } else {
                        self.fetch_reserve_raw(&state).await?
                    };
                anyhow::ensure!(
                    reserve_a_raw > 0 && reserve_b_raw > 0,
                    "meteora damm v2 pool has no liquidity"
                );

                let decimals_a = self
                    .sol
                    .get_token_decimals(&state.token_a_mint)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to fetch token-a decimals for mint {}",
                            state.token_a_mint
                        )
                    })?;
                let decimals_b = self
                    .sol
                    .get_token_decimals(&state.token_b_mint)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to fetch token-b decimals for mint {}",
                            state.token_b_mint
                        )
                    })?;

                let reserve_a = reserve_a_raw as f64 / 10_f64.powi(decimals_a as i32);
                let reserve_b = reserve_b_raw as f64 / 10_f64.powi(decimals_b as i32);
                let (sol_reserve, token_reserve) = if state.token_a_mint == WSOL_MINT {
                    (reserve_a, reserve_b)
                } else {
                    (reserve_b, reserve_a)
                };
                anyhow::ensure!(token_reserve > 0.0, "invalid meteora damm v2 token reserve");
                sol_reserve / token_reserve
            }
        };

        anyhow::ensure!(
            price.is_finite() && price > 0.0,
            "invalid meteora damm v2 token price"
        );

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
        let use_helius_pagination = Self::is_helius_rpc_url(&self.sol.rpc_client.url());
        let pools_a = if use_helius_pagination {
            match self
                .find_pool_pubkeys_by_mint_paginated_helius(DAMM_V2_POOL_TOKEN_A_MINT_OFFSET, mint)
                .await
            {
                Ok(pools) => pools,
                Err(error) => {
                    warn!(
                        "meteora damm v2 Helius paginated lookup failed for {} at token-a offset {}: {}; falling back to standard getProgramAccounts",
                        mint, DAMM_V2_POOL_TOKEN_A_MINT_OFFSET, error
                    );
                    match self
                        .find_pool_pubkeys_by_mint_standard_rpc(
                            DAMM_V2_POOL_TOKEN_A_MINT_OFFSET,
                            mint,
                        )
                        .await
                    {
                        Ok(pools) => pools,
                        Err(fallback_error) => {
                            warn!(
                                "meteora damm v2 standard getProgramAccounts fallback also failed for {} at token-a offset {}: {}; retrying via readonly public rpc",
                                mint, DAMM_V2_POOL_TOKEN_A_MINT_OFFSET, fallback_error
                            );
                            self.find_pool_pubkeys_by_mint_public_fallback(
                                DAMM_V2_POOL_TOKEN_A_MINT_OFFSET,
                                mint,
                            )
                            .await?
                        }
                    }
                }
            }
        } else {
            self.find_pool_pubkeys_by_mint_standard_rpc(DAMM_V2_POOL_TOKEN_A_MINT_OFFSET, mint)
                .await?
        };

        let pools_b = if use_helius_pagination {
            match self
                .find_pool_pubkeys_by_mint_paginated_helius(DAMM_V2_POOL_TOKEN_B_MINT_OFFSET, mint)
                .await
            {
                Ok(pools) => pools,
                Err(error) => {
                    warn!(
                        "meteora damm v2 Helius paginated lookup failed for {} at token-b offset {}: {}; falling back to standard getProgramAccounts",
                        mint, DAMM_V2_POOL_TOKEN_B_MINT_OFFSET, error
                    );
                    match self
                        .find_pool_pubkeys_by_mint_standard_rpc(
                            DAMM_V2_POOL_TOKEN_B_MINT_OFFSET,
                            mint,
                        )
                        .await
                    {
                        Ok(pools) => pools,
                        Err(fallback_error) => {
                            warn!(
                                "meteora damm v2 standard getProgramAccounts fallback also failed for {} at token-b offset {}: {}; retrying via readonly public rpc",
                                mint, DAMM_V2_POOL_TOKEN_B_MINT_OFFSET, fallback_error
                            );
                            self.find_pool_pubkeys_by_mint_public_fallback(
                                DAMM_V2_POOL_TOKEN_B_MINT_OFFSET,
                                mint,
                            )
                            .await?
                        }
                    }
                }
            }
        } else {
            self.find_pool_pubkeys_by_mint_standard_rpc(DAMM_V2_POOL_TOKEN_B_MINT_OFFSET, mint)
                .await?
        };

        let mut out = BTreeSet::new();
        for pool in pools_a.into_iter().chain(pools_b.into_iter()) {
            out.insert(pool);
        }

        if let Some(quote) = quote_mint {
            let mut filtered = Vec::new();
            let pools = out.into_iter().collect::<Vec<_>>();
            for (pool, state) in self.fetch_states_for_pools(&pools).await? {
                let matches_quote = (state.token_a_mint == *mint && state.token_b_mint == *quote)
                    || (state.token_b_mint == *mint && state.token_a_mint == *quote);
                if matches_quote {
                    filtered.push(pool);
                }
            }
            return Ok(filtered);
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

        for (pool, state) in self.fetch_states_for_pools(&pools).await? {
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
        anyhow::ensure!(price > 0.0, "meteora damm v2 buy price must be > 0");
        anyhow::ensure!(
            sol_amount_in > 0.0,
            "meteora damm v2 buy amount must be > 0"
        );

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch meteora damm v2 pool state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;
        anyhow::ensure!(
            state.pool_status == POOL_STATUS_ENABLED,
            "meteora damm v2 pool is disabled"
        );

        let token_a_program = self
            .sol
            .get_token_program_id(&state.token_a_mint)
            .await
            .context("failed to resolve token-a program for meteora damm v2")?;
        let token_b_program = self
            .sol
            .get_token_program_id(&state.token_b_mint)
            .await
            .context("failed to resolve token-b program for meteora damm v2")?;

        let (input_program, output_program) = if state.token_a_mint == WSOL_MINT {
            (token_a_program, token_b_program)
        } else {
            (token_b_program, token_a_program)
        };

        let input_ata = Self::ata_for(&buyer, &WSOL_MINT, &input_program);
        let output_ata = Self::ata_for(&buyer, mint, &output_program);

        let use_idempotent = Self::buy_use_idempotent(use_idempotent);
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
        anyhow::ensure!(amount_in > 0, "meteora damm v2 buy amount is too small");

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

        let pool_authority = Self::derive_pool_authority_pda();
        let event_authority = Self::derive_event_authority_pda();
        let mut accounts = vec![
            AccountMeta::new_readonly(pool_authority, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(state.token_a_vault, false),
            AccountMeta::new(state.token_b_vault, false),
            AccountMeta::new_readonly(state.token_a_mint, false),
            AccountMeta::new_readonly(state.token_b_mint, false),
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(token_a_program, false),
            AccountMeta::new_readonly(token_b_program, false),
            AccountMeta::new(METEORA_DAMM_V2_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DAMM_V2_ID, false),
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
            .context("failed to resolve priority fee for meteora damm v2 buy")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_instruction_data(amount_in, min_amount_out);
        ixs.push(Instruction {
            program_id: METEORA_DAMM_V2_ID,
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
        anyhow::ensure!(price > 0.0, "meteora damm v2 sell price must be > 0");

        let buyer = *buyer;
        let state = self
            .fetch_state(pool)
            .await
            .with_context(|| format!("failed to fetch meteora damm v2 pool state for {}", pool))?;
        Self::validate_pool_contains_mint(&state, mint)?;
        anyhow::ensure!(
            state.pool_status == POOL_STATUS_ENABLED,
            "meteora damm v2 pool is disabled"
        );

        let token_a_program = self
            .sol
            .get_token_program_id(&state.token_a_mint)
            .await
            .context("failed to resolve token-a program for meteora damm v2")?;
        let token_b_program = self
            .sol
            .get_token_program_id(&state.token_b_mint)
            .await
            .context("failed to resolve token-b program for meteora damm v2")?;

        let (input_program, wsol_program) = if state.token_a_mint == WSOL_MINT {
            (token_b_program, token_a_program)
        } else {
            (token_a_program, token_b_program)
        };

        let sell_pct = sell_pct.clamp(1, 100);
        let input_ata = Self::ata_for(&buyer, mint, &input_program);
        let output_ata = Self::ata_for(&buyer, &WSOL_MINT, &wsol_program);

        let token_balance_raw = self
            .user_token_balance_raw(&buyer, mint)
            .await
            .context("failed to fetch token balance for meteora damm v2 sell")?;
        anyhow::ensure!(
            token_balance_raw > 0,
            "no token balance for meteora damm v2 sell"
        );

        let amount_in = token_balance_raw.saturating_mul(sell_pct) / 100;
        anyhow::ensure!(
            amount_in > 0,
            "meteora damm v2 sell amount is too small for requested percentage"
        );

        let mint_decimals = self
            .sol
            .get_token_decimals(mint)
            .await
            .with_context(|| format!("failed to resolve decimals for mint {}", mint))?;
        let slippage_pct = Self::normalize_slippage(slippage);
        let amount_in_ui = amount_in as f64 / 10_f64.powi(mint_decimals as i32);
        let min_sol_output = (amount_in_ui * price * (1.0 - slippage_pct) * 1e9).floor() as u64;
        let min_sol_output = min_sol_output.max(1);

        let mut ixs = vec![create_associated_token_account_idempotent(
            &buyer,
            &buyer,
            &WSOL_MINT,
            &wsol_program,
        )];

        let pool_authority = Self::derive_pool_authority_pda();
        let event_authority = Self::derive_event_authority_pda();
        let mut accounts = vec![
            AccountMeta::new_readonly(pool_authority, false),
            AccountMeta::new(*pool, false),
            AccountMeta::new(input_ata, false),
            AccountMeta::new(output_ata, false),
            AccountMeta::new(state.token_a_vault, false),
            AccountMeta::new(state.token_b_vault, false),
            AccountMeta::new_readonly(state.token_a_mint, false),
            AccountMeta::new_readonly(state.token_b_mint, false),
            AccountMeta::new_readonly(buyer, true),
            AccountMeta::new_readonly(token_a_program, false),
            AccountMeta::new_readonly(token_b_program, false),
            AccountMeta::new(METEORA_DAMM_V2_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DAMM_V2_ID, false),
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
            .context("failed to resolve priority fee for meteora damm v2 sell")?;
        log!(cc::LIGHT_CYAN, "Fee: {:?}", recent_fees);

        let data = Self::encode_swap_instruction_data(amount_in, min_sol_output);
        ixs.push(Instruction {
            program_id: METEORA_DAMM_V2_ID,
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
                    .close_token_account_ix(&wsol_program, &output_ata, &buyer, &buyer)?;
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

    fn synthetic_pool_account_bytes() -> Vec<u8> {
        let mut data = vec![0u8; DAMM_V2_POOL_ACCOUNT_LEN];
        data[..8].copy_from_slice(&POOL_DISCRIM);

        let token_a_mint = Pubkey::new_unique();
        let token_b_mint = WSOL_MINT;
        let token_a_vault = Pubkey::new_unique();
        let token_b_vault = Pubkey::new_unique();
        let whitelisted_vault = Pubkey::new_unique();
        let partner = Pubkey::new_unique();
        let creator = Pubkey::new_unique();

        data[DAMM_V2_POOL_TOKEN_A_MINT_OFFSET..DAMM_V2_POOL_TOKEN_A_MINT_OFFSET + 32]
            .copy_from_slice(token_a_mint.as_ref());
        data[DAMM_V2_POOL_TOKEN_B_MINT_OFFSET..DAMM_V2_POOL_TOKEN_B_MINT_OFFSET + 32]
            .copy_from_slice(token_b_mint.as_ref());
        data[DAMM_V2_POOL_A_VAULT_OFFSET..DAMM_V2_POOL_A_VAULT_OFFSET + 32]
            .copy_from_slice(token_a_vault.as_ref());
        data[DAMM_V2_POOL_B_VAULT_OFFSET..DAMM_V2_POOL_B_VAULT_OFFSET + 32]
            .copy_from_slice(token_b_vault.as_ref());
        data[DAMM_V2_POOL_WHITELISTED_VAULT_OFFSET..DAMM_V2_POOL_WHITELISTED_VAULT_OFFSET + 32]
            .copy_from_slice(whitelisted_vault.as_ref());
        data[DAMM_V2_POOL_PARTNER_OFFSET..DAMM_V2_POOL_PARTNER_OFFSET + 32]
            .copy_from_slice(partner.as_ref());
        data[DAMM_V2_POOL_CREATOR_OFFSET..DAMM_V2_POOL_CREATOR_OFFSET + 32]
            .copy_from_slice(creator.as_ref());

        data[DAMM_V2_POOL_LIQUIDITY_OFFSET..DAMM_V2_POOL_LIQUIDITY_OFFSET + 16]
            .copy_from_slice(&123_456_789u128.to_le_bytes());
        data[DAMM_V2_POOL_PROTOCOL_A_FEE_OFFSET..DAMM_V2_POOL_PROTOCOL_A_FEE_OFFSET + 8]
            .copy_from_slice(&100u64.to_le_bytes());
        data[DAMM_V2_POOL_PROTOCOL_B_FEE_OFFSET..DAMM_V2_POOL_PROTOCOL_B_FEE_OFFSET + 8]
            .copy_from_slice(&101u64.to_le_bytes());
        data[DAMM_V2_POOL_PARTNER_A_FEE_OFFSET..DAMM_V2_POOL_PARTNER_A_FEE_OFFSET + 8]
            .copy_from_slice(&102u64.to_le_bytes());
        data[DAMM_V2_POOL_PARTNER_B_FEE_OFFSET..DAMM_V2_POOL_PARTNER_B_FEE_OFFSET + 8]
            .copy_from_slice(&103u64.to_le_bytes());
        data[DAMM_V2_POOL_SQRT_MIN_PRICE_OFFSET..DAMM_V2_POOL_SQRT_MIN_PRICE_OFFSET + 16]
            .copy_from_slice(&1000u128.to_le_bytes());
        data[DAMM_V2_POOL_SQRT_MAX_PRICE_OFFSET..DAMM_V2_POOL_SQRT_MAX_PRICE_OFFSET + 16]
            .copy_from_slice(&2000u128.to_le_bytes());
        data[DAMM_V2_POOL_SQRT_PRICE_OFFSET..DAMM_V2_POOL_SQRT_PRICE_OFFSET + 16]
            .copy_from_slice(&(1u128 << 64).to_le_bytes());
        data[DAMM_V2_POOL_ACTIVATION_POINT_OFFSET..DAMM_V2_POOL_ACTIVATION_POINT_OFFSET + 8]
            .copy_from_slice(&1700000000u64.to_le_bytes());

        data[DAMM_V2_POOL_ACTIVATION_TYPE_OFFSET] = 1;
        data[DAMM_V2_POOL_STATUS_OFFSET] = POOL_STATUS_ENABLED;
        data[DAMM_V2_POOL_TOKEN_A_FLAG_OFFSET] = 0;
        data[DAMM_V2_POOL_TOKEN_B_FLAG_OFFSET] = 0;
        data[DAMM_V2_POOL_COLLECT_FEE_MODE_OFFSET] = 1;
        data[DAMM_V2_POOL_TYPE_OFFSET] = 2;
        data[DAMM_V2_POOL_VERSION_OFFSET] = 1;
        data[DAMM_V2_POOL_TOKEN_A_AMOUNT_OFFSET..DAMM_V2_POOL_TOKEN_A_AMOUNT_OFFSET + 8]
            .copy_from_slice(&5_000u64.to_le_bytes());
        data[DAMM_V2_POOL_TOKEN_B_AMOUNT_OFFSET..DAMM_V2_POOL_TOKEN_B_AMOUNT_OFFSET + 8]
            .copy_from_slice(&10_000_000_000u64.to_le_bytes());
        data[DAMM_V2_POOL_LAYOUT_VERSION_OFFSET] = 1;

        data
    }

    #[test]
    fn test_meteora_damm_v2_discriminators_match_anchor_layout() {
        assert_eq!(SWAP_IX_DISCRIM, anchor_discriminator("global", "swap"));
        assert_eq!(POOL_DISCRIM, anchor_discriminator("account", "Pool"));
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
    fn test_meteora_damm_v2_program_constants() {
        assert_eq!(
            METEORA_DAMM_V2_ID,
            Pubkey::from_str("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG").unwrap()
        );
        assert_eq!(
            MeteoraDammV2::derive_pool_authority_pda(),
            METEORA_DAMM_V2_POOL_AUTHORITY
        );
    }

    #[test]
    fn test_meteora_damm_v2_swap_instruction_data_encoding() {
        let data = MeteoraDammV2::encode_swap_instruction_data(1_234, 9_876);
        assert_eq!(&data[..8], &SWAP_IX_DISCRIM);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 1_234);
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 9_876);
    }

    #[test]
    fn test_meteora_damm_v2_normalize_slippage() {
        assert_eq!(MeteoraDammV2::normalize_slippage(0.15), 0.15);
        assert_eq!(MeteoraDammV2::normalize_slippage(15.0), 0.15);
        assert_eq!(MeteoraDammV2::normalize_slippage(0.0), 0.01);
        assert_eq!(MeteoraDammV2::normalize_slippage(150.0), 0.99);
    }

    #[test]
    fn test_meteora_damm_v2_buy_defaults_to_idempotent_ata_creation() {
        assert!(MeteoraDammV2::buy_use_idempotent(None));
        assert!(MeteoraDammV2::buy_use_idempotent(Some(true)));
        assert!(!MeteoraDammV2::buy_use_idempotent(Some(false)));
    }

    #[test]
    fn test_meteora_damm_v2_price_from_sqrt_price_x64() {
        let mut state = MeteoraDammV2::decode_pool_account_data(&synthetic_pool_account_bytes())
            .expect("synthetic state must decode");
        state.token_b_mint = WSOL_MINT;
        state.token_a_mint = Pubkey::new_unique();
        state.sqrt_price = 1u128 << 64;

        let price = MeteoraDammV2::price_from_sqrt_price_x64(&state, 6).unwrap();
        assert!((price - 0.001).abs() < 1e-12);
    }

    #[test]
    fn test_meteora_damm_v2_decode_pool_layout_contract() {
        let data = synthetic_pool_account_bytes();
        let state = MeteoraDammV2::decode_pool_account_data(&data).expect("state must decode");

        assert_ne!(state.token_a_mint, Pubkey::default());
        assert_eq!(state.token_b_mint, WSOL_MINT);
        assert_ne!(state.token_a_vault, Pubkey::default());
        assert_ne!(state.token_b_vault, Pubkey::default());
        assert_eq!(state.pool_status, POOL_STATUS_ENABLED);
        assert_eq!(state.collect_fee_mode, 1);
        assert_eq!(state.pool_type, 2);
        assert_eq!(state.version, 1);
    }

    #[test]
    fn test_meteora_damm_v2_parse_logs_ignores_invalid_payloads() {
        let logs = vec![
            "Program data: not_base64".to_string(),
            "Program data: AQI=".to_string(),
        ];
        let events = MeteoraDammV2::parse_logs(logs.iter(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_meteora_damm_v2_parse_logs_unknown_discriminator() {
        let payload = B64.encode([0u8; 32]);
        let logs = vec![format!("Program data: {payload}")];
        let events = MeteoraDammV2::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MeteoraDammV2Event::Unknown));
    }

    #[test]
    fn test_meteora_damm_v2_parse_logs_initialize_pool_fixture_decodes() {
        let fixture = InitializePoolEvent {
            pool: Pubkey::new_unique(),
            token_a_mint: Pubkey::new_unique(),
            token_b_mint: WSOL_MINT,
            creator: Pubkey::new_unique(),
        };
        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(
                &EVT_INITIALIZE_POOL_EVENT_DISCRIM,
                &fixture.to_bytes_with_padding()
            )
        )];

        let events = MeteoraDammV2::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDammV2Event::InitializePool(Some(event)) => {
                assert_eq!(event.pool, fixture.pool);
                assert_eq!(event.token_a_mint, fixture.token_a_mint);
                assert_eq!(event.token_b_mint, fixture.token_b_mint);
                assert_eq!(event.creator, fixture.creator);
            }
            _ => panic!("expected parsed meteora damm v2 initialize-pool event"),
        }
    }

    #[test]
    fn test_meteora_damm_v2_parse_logs_swap2_fixture_decodes() {
        let fixture = Swap2Event {
            pool: Pubkey::new_unique(),
            trade_direction: 1,
            collect_fee_mode: 0,
            has_referral: true,
            amount_0: 1_000,
            amount_1: 900,
            swap_mode: 0,
            included_fee_input_amount: 777,
            excluded_fee_input_amount: 666,
            output_amount: 850,
            reserve_a_amount: 10_000,
            reserve_b_amount: 20_000,
        };
        let logs = vec![format!(
            "Program data: {}",
            encode_fixture_event(&EVT_SWAP2_EVENT_DISCRIM, &fixture.to_bytes())
        )];

        let events = MeteoraDammV2::parse_logs(logs.iter(), None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeteoraDammV2Event::Swap2(Some(event)) => {
                assert_eq!(event.pool, fixture.pool);
                assert_eq!(event.trade_direction, fixture.trade_direction);
                assert_eq!(event.collect_fee_mode, fixture.collect_fee_mode);
                assert_eq!(event.has_referral, fixture.has_referral);
                assert_eq!(event.amount_0, fixture.amount_0);
                assert_eq!(event.amount_1, fixture.amount_1);
                assert_eq!(event.swap_mode, fixture.swap_mode);
                assert_eq!(
                    event.included_fee_input_amount,
                    fixture.included_fee_input_amount
                );
                assert_eq!(
                    event.excluded_fee_input_amount,
                    fixture.excluded_fee_input_amount
                );
                assert_eq!(event.output_amount, fixture.output_amount);
                assert_eq!(event.reserve_a_amount, fixture.reserve_a_amount);
                assert_eq!(event.reserve_b_amount, fixture.reserve_b_amount);
            }
            _ => panic!("expected parsed meteora damm v2 swap2 event"),
        }
    }

    #[test]
    fn test_meteora_damm_v2_extract_pool_from_inner_instruction_fixture() {
        let pool = "8Pm2kZpnxD3hoMmt4bjStX2Pw2Z9abpbHzZxMPqxPmie";
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
                            "pubkey": METEORA_DAMM_V2_POOL_AUTHORITY.to_string(),
                            "writable": false,
                            "signer": false
                        },
                        {
                            "pubkey": pool,
                            "writable": true,
                            "signer": false
                        },
                        {
                            "pubkey": METEORA_DAMM_V2_ID.to_string(),
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
                "preBalances": [0, 0, 0, 0],
                "postBalances": [0, 0, 0, 0],
                "innerInstructions": [
                    {
                        "index": 0,
                        "instructions": [
                            {
                                "programId": METEORA_DAMM_V2_ID.to_string(),
                                "accounts": [
                                    METEORA_DAMM_V2_POOL_AUTHORITY.to_string(),
                                    pool,
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

        assert_eq!(
            MeteoraDammV2::extract_pool_from_transaction(&tx),
            Some(Pubkey::from_str(pool).unwrap())
        );
    }

    #[test]
    fn test_meteora_damm_v2_extract_pool_from_initialize_pool_logs_fixture() {
        let fixture = InitializePoolEvent {
            pool: Pubkey::new_unique(),
            token_a_mint: Pubkey::new_unique(),
            token_b_mint: WSOL_MINT,
            creator: Pubkey::new_unique(),
        };
        let tx_json = json!({
            "slot": 1,
            "transaction": {
                "signatures": [
                    "1111111111111111111111111111111111111111111111111111111111111111"
                ],
                "message": {
                    "accountKeys": [
                        {
                            "pubkey": METEORA_DAMM_V2_ID.to_string(),
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
                "innerInstructions": [],
                "logMessages": [
                    format!(
                        "{}{}",
                        SEARCH_FOR,
                        encode_fixture_event(
                            &EVT_INITIALIZE_POOL_EVENT_DISCRIM,
                            &fixture.to_bytes_with_padding()
                        )
                    )
                ]
            },
            "version": "legacy",
            "blockTime": null
        });

        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(tx_json).expect("fixture tx must deserialize");

        assert_eq!(
            MeteoraDammV2::extract_pool_from_transaction(&tx),
            Some(fixture.pool)
        );
    }
}
