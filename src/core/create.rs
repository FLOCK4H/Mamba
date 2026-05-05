use {
    crate::compute_budget::compute_budget::{ix_set_compute_unit_limit, ix_set_compute_unit_price},
    crate::core::sol::{
        ATA_PROGRAM_ID, METADATA_PROGRAM_ID, SYSTEM_PROGRAM, TOKEN_2022_PROGRAM_ID,
        TOKEN_PROGRAM_ID,
    },
    crate::dex::{
        pump_fun::{
            BUY_DISCRIM, BUY_EXACT_SOL_IN_DISCRIM, CREATE_V2_IX_DISCRIM, EXTEND_ACCOUNT_IX_DISCRIM,
            FEE_CONFIG, FEE_PROGRAM, GLOBAL_VOLUME_ACCUMULATOR, MAYHEM_PROGRAM_ID, PUMP_FUN_ID,
        },
        raydium_launchpad::{LAUNCHPAD_AUTH_SEED, LAUNCHPAD_EVENT_AUTH_SEED},
    },
    anchor_lang::{InstructionData, ToAccountMetas},
    anyhow::Context,
    base64::{Engine as _, engine::general_purpose::STANDARD as B64},
    raydium_launchpad_types as raydium_launchpad_idl,
    solana_message::{VersionedMessage, v0::Message as V0Message},
    solana_program::{
        hash::Hash,
        instruction::{AccountMeta, Instruction},
        program_pack::Pack,
        pubkey::Pubkey,
    },
    solana_signature::Signature,
    solana_system_interface::instruction as system_instruction_if,
    solana_transaction::versioned::VersionedTransaction,
    spl_associated_token_account::instruction::create_associated_token_account,
    spl_associated_token_account::instruction::create_associated_token_account_idempotent,
    spl_token::state::Mint as SplMint,
    spl_token_2022::{
        extension::ExtensionType,
        instruction::sync_native,
        state::{Mint as SplMint2022, Multisig as SplMultisig},
    },
    spl_token_metadata_interface::state::TokenMetadata as Token2022Metadata,
    std::collections::BTreeMap,
};

pub const RENT_SYSVAR_ID: Pubkey =
    Pubkey::from_str_const("SysvarRent111111111111111111111111111111111");

const PUMP_FUN_CREATE_IX_DISCRIM: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];
const MPL_CREATE_METADATA_ACCOUNT_V3_DISCRIM: u8 = 33;
#[cfg(test)]
const RAYDIUM_LAUNCHPAD_INITIALIZE_V2_IX_DISCRIM: [u8; 8] = [67, 153, 175, 39, 218, 16, 38, 32];
#[cfg(test)]
const RAYDIUM_LAUNCHPAD_INITIALIZE_WITH_TOKEN_2022_IX_DISCRIM: [u8; 8] =
    [37, 190, 126, 222, 44, 154, 171, 17];
const RAYDIUM_LAUNCHPAD_POOL_SEED: &[u8] = b"pool";
const RAYDIUM_LAUNCHPAD_POOL_VAULT_SEED: &[u8] = b"pool_vault";
pub const SOLANA_MAX_TX_WIRE_BYTES: usize = 1232;

#[derive(Debug, Clone, Copy)]
pub struct ComputeBudgetPlan {
    pub compute_unit_price_micro_lamports: Option<u64>,
    pub compute_unit_limit: Option<u32>,
}

impl ComputeBudgetPlan {
    pub fn prepend_instructions(&self, instructions: &mut Vec<Instruction>) {
        if let Some(price) = self.compute_unit_price_micro_lamports {
            instructions.insert(0, ix_set_compute_unit_price(price));
        }
        if let Some(limit) = self.compute_unit_limit {
            let idx = if self.compute_unit_price_micro_lamports.is_some() {
                1
            } else {
                0
            };
            instructions.insert(idx, ix_set_compute_unit_limit(limit));
        }
    }
}

#[derive(Debug, Clone)]
pub struct DerivedAddresses {
    pub map: BTreeMap<String, Pubkey>,
}

impl DerivedAddresses {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    pub fn insert(mut self, label: impl Into<String>, address: Pubkey) -> Self {
        self.map.insert(label.into(), address);
        self
    }
}

impl Default for DerivedAddresses {
    fn default() -> Self {
        Self::new()
    }
}

pub fn derive_associated_token_address(
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ATA_PROGRAM_ID,
    )
    .0
}

pub fn derive_metaplex_metadata_pda(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"metadata", METADATA_PROGRAM_ID.as_ref(), mint.as_ref()],
        &METADATA_PROGRAM_ID,
    )
    .0
}

#[derive(Debug, Clone)]
pub struct PlannedCreateTx {
    pub payer: Pubkey,
    pub required_signers: Vec<Pubkey>,
    pub derived: DerivedAddresses,
    pub priority_fee_addresses: Vec<Pubkey>,
    pub instructions: Vec<Instruction>,
}

pub fn compile_unsigned_v0_transaction(
    payer: &Pubkey,
    instructions: &[Instruction],
    blockhash: Hash,
) -> anyhow::Result<VersionedTransaction> {
    let msg = VersionedMessage::V0(
        V0Message::try_compile(payer, instructions, &[], blockhash)
            .context("failed to compile v0 message")?,
    );
    let required = msg.header().num_required_signatures as usize;
    Ok(VersionedTransaction {
        signatures: vec![Signature::default(); required],
        message: msg,
    })
}

pub fn encode_transaction_base64(tx: &VersionedTransaction) -> anyhow::Result<String> {
    let wire = bincode::serialize(tx).context("failed to bincode serialize transaction")?;
    Ok(B64.encode(wire))
}

#[derive(Debug, Clone)]
pub struct PumpFunCreatePlan {
    pub payer: Pubkey,
    pub mint: Pubkey,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub is_mayhem_mode: bool,
}

#[derive(Debug, Clone)]
pub struct PumpFunAutoBuyPlan {
    pub amount: u64,
    pub max_sol_cost: u64,
    pub fee_recipient: Pubkey,
    pub track_volume: bool,
}

#[derive(Debug, Clone)]
pub struct PumpFunAutoBuyExactSolInPlan {
    pub spendable_sol_in: u64,
    pub min_tokens_out: u64,
    pub fee_recipient: Pubkey,
    pub track_volume: bool,
}

#[derive(Debug, Clone)]
pub struct SplTokenCreatePlan {
    pub payer: Pubkey,
    pub mint: Pubkey,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub decimals: u8,
    pub initial_supply: u64,
    pub freeze_authority: bool,
    pub revoke_mint_authority: bool,
    pub revoke_freeze_authority: bool,
    pub metadata_is_mutable: bool,
}

#[derive(Debug, Clone)]
pub struct SplToken2022CreatePlan {
    pub payer: Pubkey,
    pub mint: Pubkey,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub decimals: u8,
    pub initial_supply: u64,
    pub freeze_authority: bool,
    pub revoke_mint_authority: bool,
    pub revoke_freeze_authority: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum RaydiumLaunchpadBaseTokenProgram {
    Token,
    Token2022,
}

#[derive(Debug, Clone)]
pub enum RaydiumLaunchpadCurveParams {
    Constant {
        supply: u64,
        total_base_sell: u64,
        total_quote_fund_raising: u64,
        migrate_type: u8,
    },
    Fixed {
        supply: u64,
        total_quote_fund_raising: u64,
        migrate_type: u8,
    },
    Linear {
        supply: u64,
        total_quote_fund_raising: u64,
        migrate_type: u8,
    },
}

impl RaydiumLaunchpadCurveParams {
    pub fn migrate_type(&self) -> u8 {
        match self {
            Self::Constant { migrate_type, .. }
            | Self::Fixed { migrate_type, .. }
            | Self::Linear { migrate_type, .. } => *migrate_type,
        }
    }

    #[cfg(test)]
    fn encode_borsh(&self, out: &mut Vec<u8>) {
        match self {
            Self::Constant {
                supply,
                total_base_sell,
                total_quote_fund_raising,
                migrate_type,
            } => {
                out.push(0u8);
                out.extend_from_slice(&supply.to_le_bytes());
                out.extend_from_slice(&total_base_sell.to_le_bytes());
                out.extend_from_slice(&total_quote_fund_raising.to_le_bytes());
                out.push(*migrate_type);
            }
            Self::Fixed {
                supply,
                total_quote_fund_raising,
                migrate_type,
            } => {
                out.push(1u8);
                out.extend_from_slice(&supply.to_le_bytes());
                out.extend_from_slice(&total_quote_fund_raising.to_le_bytes());
                out.push(*migrate_type);
            }
            Self::Linear {
                supply,
                total_quote_fund_raising,
                migrate_type,
            } => {
                out.push(2u8);
                out.extend_from_slice(&supply.to_le_bytes());
                out.extend_from_slice(&total_quote_fund_raising.to_le_bytes());
                out.push(*migrate_type);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadVestingParams {
    pub total_locked_amount: u64,
    pub cliff_period: u64,
    pub unlock_period: u64,
}

impl RaydiumLaunchpadVestingParams {
    #[cfg(test)]
    fn encode_borsh(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.total_locked_amount.to_le_bytes());
        out.extend_from_slice(&self.cliff_period.to_le_bytes());
        out.extend_from_slice(&self.unlock_period.to_le_bytes());
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RaydiumLaunchpadAmmFeeOn {
    QuoteToken,
    BothToken,
}

impl RaydiumLaunchpadAmmFeeOn {
    #[cfg(test)]
    fn encode_borsh(&self, out: &mut Vec<u8>) {
        match self {
            Self::QuoteToken => out.push(0u8),
            Self::BothToken => out.push(1u8),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadTransferFeeExtensionParams {
    pub transfer_fee_basis_points: u16,
    pub maximum_fee: u64,
}

impl RaydiumLaunchpadTransferFeeExtensionParams {
    #[cfg(test)]
    fn encode_borsh(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.transfer_fee_basis_points.to_le_bytes());
        out.extend_from_slice(&self.maximum_fee.to_le_bytes());
    }
}

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadCreatePlan {
    pub launchpad_program_id: Pubkey,
    pub payer: Pubkey,
    pub creator: Pubkey,
    pub global_config: Pubkey,
    pub platform_config: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub decimals: u8,
    pub curve: RaydiumLaunchpadCurveParams,
    pub vesting: RaydiumLaunchpadVestingParams,
    pub amm_fee_on: RaydiumLaunchpadAmmFeeOn,
    pub base_token_program: RaydiumLaunchpadBaseTokenProgram,
    pub transfer_fee_extension: Option<RaydiumLaunchpadTransferFeeExtensionParams>,
}

#[derive(Debug, Clone)]
pub struct RaydiumLaunchpadAutoBuyExactSolInPlan {
    pub amount_in_quote_lamports: u64,
    pub min_base_amount_out: u64,
    pub share_fee_rate: u64,
}

fn spl_token_set_authority_ix(
    mint: &Pubkey,
    payer: &Pubkey,
    authority_type: spl_token::instruction::AuthorityType,
) -> anyhow::Result<Instruction> {
    spl_token::instruction::set_authority(&TOKEN_PROGRAM_ID, mint, None, authority_type, payer, &[])
        .map_err(|e| anyhow::anyhow!("failed to build SPL Token set_authority instruction: {e:?}"))
}

fn spl_token_2022_set_authority_ix(
    mint: &Pubkey,
    payer: &Pubkey,
    authority_type: spl_token_2022::instruction::AuthorityType,
) -> anyhow::Result<Instruction> {
    spl_token_2022::instruction::set_authority(
        &TOKEN_2022_PROGRAM_ID,
        mint,
        None,
        authority_type,
        payer,
        &[],
    )
    .map_err(|e| anyhow::anyhow!("failed to build Token-2022 set_authority instruction: {e:?}"))
}

pub fn plan_pump_fun_create(
    plan: PumpFunCreatePlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedCreateTx> {
    if plan.is_mayhem_mode {
        return plan_pump_fun_create_v2(plan, compute_budget);
    }

    let global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
    let mint_authority = Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
    let bonding_curve =
        Pubkey::find_program_address(&[b"bonding-curve", plan.mint.as_ref()], &PUMP_FUN_ID).0;
    let associated_bonding_curve =
        derive_associated_token_address(&bonding_curve, &plan.mint, &TOKEN_PROGRAM_ID);
    let metadata = derive_metaplex_metadata_pda(&plan.mint);
    let event_authority = Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;

    let mut data =
        Vec::with_capacity(8 + plan.name.len() + plan.symbol.len() + plan.uri.len() + 32 + 16);
    data.extend_from_slice(&PUMP_FUN_CREATE_IX_DISCRIM);
    encode_borsh_string(&plan.name, &mut data);
    encode_borsh_string(&plan.symbol, &mut data);
    encode_borsh_string(&plan.uri, &mut data);
    data.extend_from_slice(plan.payer.as_ref());

    let mut instructions = vec![Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(plan.mint, true),
            AccountMeta::new_readonly(mint_authority, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new_readonly(global, false),
            AccountMeta::new_readonly(METADATA_PROGRAM_ID, false),
            AccountMeta::new(metadata, false),
            AccountMeta::new(plan.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data,
    }];
    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedCreateTx {
        payer: plan.payer,
        required_signers: vec![plan.payer, plan.mint],
        derived: DerivedAddresses::new()
            .insert("global", global)
            .insert("mint_authority", mint_authority)
            .insert("bonding_curve", bonding_curve)
            .insert("associated_bonding_curve", associated_bonding_curve)
            .insert("metadata", metadata)
            .insert("event_authority", event_authority),
        priority_fee_addresses: vec![PUMP_FUN_ID, TOKEN_PROGRAM_ID, METADATA_PROGRAM_ID],
        instructions,
    })
}

pub fn plan_pump_fun_create_v2(
    plan: PumpFunCreatePlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedCreateTx> {
    let global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
    let mint_authority = Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
    let bonding_curve =
        Pubkey::find_program_address(&[b"bonding-curve", plan.mint.as_ref()], &PUMP_FUN_ID).0;
    let associated_bonding_curve =
        derive_associated_token_address(&bonding_curve, &plan.mint, &TOKEN_2022_PROGRAM_ID);
    let event_authority = Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;
    let global_params = Pubkey::find_program_address(&[b"global-params"], &MAYHEM_PROGRAM_ID).0;
    let sol_vault = Pubkey::find_program_address(&[b"sol-vault"], &MAYHEM_PROGRAM_ID).0;
    let mayhem_state =
        Pubkey::find_program_address(&[b"mayhem-state", plan.mint.as_ref()], &MAYHEM_PROGRAM_ID).0;
    let mayhem_token_vault =
        derive_associated_token_address(&sol_vault, &plan.mint, &TOKEN_2022_PROGRAM_ID);

    let mut data =
        Vec::with_capacity(8 + plan.name.len() + plan.symbol.len() + plan.uri.len() + 32 + 2);
    data.extend_from_slice(&CREATE_V2_IX_DISCRIM);
    encode_borsh_string(&plan.name, &mut data);
    encode_borsh_string(&plan.symbol, &mut data);
    encode_borsh_string(&plan.uri, &mut data);
    data.extend_from_slice(plan.payer.as_ref());
    data.push(u8::from(plan.is_mayhem_mode));
    data.push(0);

    let create_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(plan.mint, true),
            AccountMeta::new_readonly(mint_authority, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(plan.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new(MAYHEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(global_params, false),
            AccountMeta::new(sol_vault, false),
            AccountMeta::new(mayhem_state, false),
            AccountMeta::new(mayhem_token_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data,
    };

    let extend_account_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(plan.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: EXTEND_ACCOUNT_IX_DISCRIM.to_vec(),
    };

    let mut instructions = vec![create_ix, extend_account_ix];
    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedCreateTx {
        payer: plan.payer,
        required_signers: vec![plan.payer, plan.mint],
        derived: DerivedAddresses::new()
            .insert("global", global)
            .insert("mint_authority", mint_authority)
            .insert("bonding_curve", bonding_curve)
            .insert("associated_bonding_curve", associated_bonding_curve)
            .insert("event_authority", event_authority)
            .insert("mayhem_program_id", MAYHEM_PROGRAM_ID)
            .insert("mayhem_global_params", global_params)
            .insert("mayhem_sol_vault", sol_vault)
            .insert("mayhem_state", mayhem_state)
            .insert("mayhem_token_vault", mayhem_token_vault),
        priority_fee_addresses: vec![
            PUMP_FUN_ID,
            TOKEN_2022_PROGRAM_ID,
            ATA_PROGRAM_ID,
            MAYHEM_PROGRAM_ID,
        ],
        instructions,
    })
}

pub fn plan_pump_fun_create_and_buy(
    create: PumpFunCreatePlan,
    buy: PumpFunAutoBuyPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedCreateTx> {
    if create.is_mayhem_mode {
        return plan_pump_fun_create_v2_and_buy(create, buy, compute_budget);
    }

    let global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
    let mint_authority = Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
    let bonding_curve =
        Pubkey::find_program_address(&[b"bonding-curve", create.mint.as_ref()], &PUMP_FUN_ID).0;
    let associated_bonding_curve =
        derive_associated_token_address(&bonding_curve, &create.mint, &TOKEN_PROGRAM_ID);
    let metadata = derive_metaplex_metadata_pda(&create.mint);
    let event_authority = Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;
    // Newer Pump SDKs require passing this PDA as a remaining account for buys/sells.
    let bonding_curve_v2 =
        Pubkey::find_program_address(&[b"bonding-curve-v2", create.mint.as_ref()], &PUMP_FUN_ID).0;

    let associated_user =
        derive_associated_token_address(&create.payer, &create.mint, &TOKEN_PROGRAM_ID);
    let user_volume_accumulator = Pubkey::find_program_address(
        &[b"user_volume_accumulator", create.payer.as_ref()],
        &PUMP_FUN_ID,
    )
    .0;
    let creator_vault =
        Pubkey::find_program_address(&[b"creator-vault", create.payer.as_ref()], &PUMP_FUN_ID).0;

    let mut create_data = Vec::with_capacity(
        8 + create.name.len() + create.symbol.len() + create.uri.len() + 32 + 16,
    );
    create_data.extend_from_slice(&PUMP_FUN_CREATE_IX_DISCRIM);
    encode_borsh_string(&create.name, &mut create_data);
    encode_borsh_string(&create.symbol, &mut create_data);
    encode_borsh_string(&create.uri, &mut create_data);
    create_data.extend_from_slice(create.payer.as_ref());

    let create_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(create.mint, true),
            AccountMeta::new_readonly(mint_authority, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new_readonly(global, false),
            AccountMeta::new_readonly(METADATA_PROGRAM_ID, false),
            AccountMeta::new(metadata, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: create_data,
    };

    let extend_account_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: EXTEND_ACCOUNT_IX_DISCRIM.to_vec(),
    };

    let create_ata_ix = create_associated_token_account_idempotent(
        &create.payer,
        &create.payer,
        &create.mint,
        &TOKEN_PROGRAM_ID,
    );

    let mut buy_data = Vec::with_capacity(8 + 8 + 8 + 1);
    buy_data.extend_from_slice(&BUY_DISCRIM);
    buy_data.extend_from_slice(&buy.amount.to_le_bytes());
    buy_data.extend_from_slice(&buy.max_sol_cost.to_le_bytes());
    buy_data.push(u8::from(buy.track_volume));

    let buy_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(buy.fee_recipient, false),
            AccountMeta::new_readonly(create.mint, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new(associated_user, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new(creator_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
            AccountMeta::new(GLOBAL_VOLUME_ACCUMULATOR, false),
            AccountMeta::new(user_volume_accumulator, false),
            AccountMeta::new_readonly(FEE_CONFIG, false),
            AccountMeta::new_readonly(FEE_PROGRAM, false),
            AccountMeta::new_readonly(bonding_curve_v2, false),
        ],
        data: buy_data,
    };

    let mut instructions = vec![create_ix, extend_account_ix, create_ata_ix, buy_ix];
    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedCreateTx {
        payer: create.payer,
        required_signers: vec![create.payer, create.mint],
        derived: DerivedAddresses::new()
            .insert("global", global)
            .insert("mint_authority", mint_authority)
            .insert("bonding_curve", bonding_curve)
            .insert("associated_bonding_curve", associated_bonding_curve)
            .insert("metadata", metadata)
            .insert("event_authority", event_authority)
            .insert("bonding_curve_v2", bonding_curve_v2)
            .insert("associated_user", associated_user)
            .insert("creator_vault", creator_vault)
            .insert("global_volume_accumulator", GLOBAL_VOLUME_ACCUMULATOR)
            .insert("user_volume_accumulator", user_volume_accumulator)
            .insert("fee_recipient", buy.fee_recipient)
            .insert("fee_config", FEE_CONFIG)
            .insert("fee_program", FEE_PROGRAM),
        priority_fee_addresses: vec![
            PUMP_FUN_ID,
            TOKEN_PROGRAM_ID,
            METADATA_PROGRAM_ID,
            ATA_PROGRAM_ID,
            FEE_PROGRAM,
            buy.fee_recipient,
        ],
        instructions,
    })
}

pub fn plan_pump_fun_create_and_buy_exact_sol_in(
    create: PumpFunCreatePlan,
    buy: PumpFunAutoBuyExactSolInPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedCreateTx> {
    if create.is_mayhem_mode {
        return plan_pump_fun_create_v2_and_buy_exact_sol_in(create, buy, compute_budget);
    }

    let global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
    let mint_authority = Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
    let bonding_curve =
        Pubkey::find_program_address(&[b"bonding-curve", create.mint.as_ref()], &PUMP_FUN_ID).0;
    let associated_bonding_curve =
        derive_associated_token_address(&bonding_curve, &create.mint, &TOKEN_PROGRAM_ID);
    let metadata = derive_metaplex_metadata_pda(&create.mint);
    let event_authority = Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;
    let bonding_curve_v2 =
        Pubkey::find_program_address(&[b"bonding-curve-v2", create.mint.as_ref()], &PUMP_FUN_ID).0;

    let associated_user =
        derive_associated_token_address(&create.payer, &create.mint, &TOKEN_PROGRAM_ID);
    let user_volume_accumulator = Pubkey::find_program_address(
        &[b"user_volume_accumulator", create.payer.as_ref()],
        &PUMP_FUN_ID,
    )
    .0;
    let creator_vault =
        Pubkey::find_program_address(&[b"creator-vault", create.payer.as_ref()], &PUMP_FUN_ID).0;

    let mut create_data = Vec::with_capacity(
        8 + create.name.len() + create.symbol.len() + create.uri.len() + 32 + 16,
    );
    create_data.extend_from_slice(&PUMP_FUN_CREATE_IX_DISCRIM);
    encode_borsh_string(&create.name, &mut create_data);
    encode_borsh_string(&create.symbol, &mut create_data);
    encode_borsh_string(&create.uri, &mut create_data);
    create_data.extend_from_slice(create.payer.as_ref());

    let create_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(create.mint, true),
            AccountMeta::new_readonly(mint_authority, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new_readonly(global, false),
            AccountMeta::new_readonly(METADATA_PROGRAM_ID, false),
            AccountMeta::new(metadata, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: create_data,
    };

    let extend_account_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: EXTEND_ACCOUNT_IX_DISCRIM.to_vec(),
    };

    let create_ata_ix = create_associated_token_account_idempotent(
        &create.payer,
        &create.payer,
        &create.mint,
        &TOKEN_PROGRAM_ID,
    );

    let mut buy_data = Vec::with_capacity(8 + 8 + 8 + 1);
    buy_data.extend_from_slice(&BUY_EXACT_SOL_IN_DISCRIM);
    buy_data.extend_from_slice(&buy.spendable_sol_in.to_le_bytes());
    buy_data.extend_from_slice(&buy.min_tokens_out.to_le_bytes());
    buy_data.push(u8::from(buy.track_volume));

    let buy_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(buy.fee_recipient, false),
            AccountMeta::new_readonly(create.mint, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new(associated_user, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new(creator_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
            AccountMeta::new(GLOBAL_VOLUME_ACCUMULATOR, false),
            AccountMeta::new(user_volume_accumulator, false),
            AccountMeta::new_readonly(FEE_CONFIG, false),
            AccountMeta::new_readonly(FEE_PROGRAM, false),
            AccountMeta::new_readonly(bonding_curve_v2, false),
        ],
        data: buy_data,
    };

    let mut instructions = vec![create_ix, extend_account_ix, create_ata_ix, buy_ix];
    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedCreateTx {
        payer: create.payer,
        required_signers: vec![create.payer, create.mint],
        derived: DerivedAddresses::new()
            .insert("global", global)
            .insert("mint_authority", mint_authority)
            .insert("bonding_curve", bonding_curve)
            .insert("associated_bonding_curve", associated_bonding_curve)
            .insert("metadata", metadata)
            .insert("event_authority", event_authority)
            .insert("bonding_curve_v2", bonding_curve_v2)
            .insert("associated_user", associated_user)
            .insert("creator_vault", creator_vault)
            .insert("global_volume_accumulator", GLOBAL_VOLUME_ACCUMULATOR)
            .insert("user_volume_accumulator", user_volume_accumulator)
            .insert("fee_recipient", buy.fee_recipient)
            .insert("fee_config", FEE_CONFIG)
            .insert("fee_program", FEE_PROGRAM),
        priority_fee_addresses: vec![
            PUMP_FUN_ID,
            TOKEN_PROGRAM_ID,
            METADATA_PROGRAM_ID,
            ATA_PROGRAM_ID,
            FEE_PROGRAM,
            buy.fee_recipient,
        ],
        instructions,
    })
}

pub fn plan_pump_fun_create_v2_and_buy(
    create: PumpFunCreatePlan,
    buy: PumpFunAutoBuyPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedCreateTx> {
    let global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
    let mint_authority = Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
    let bonding_curve =
        Pubkey::find_program_address(&[b"bonding-curve", create.mint.as_ref()], &PUMP_FUN_ID).0;
    let associated_bonding_curve =
        derive_associated_token_address(&bonding_curve, &create.mint, &TOKEN_2022_PROGRAM_ID);
    let event_authority = Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;
    let bonding_curve_v2 =
        Pubkey::find_program_address(&[b"bonding-curve-v2", create.mint.as_ref()], &PUMP_FUN_ID).0;

    let associated_user =
        derive_associated_token_address(&create.payer, &create.mint, &TOKEN_2022_PROGRAM_ID);
    let user_volume_accumulator = Pubkey::find_program_address(
        &[b"user_volume_accumulator", create.payer.as_ref()],
        &PUMP_FUN_ID,
    )
    .0;
    let creator_vault =
        Pubkey::find_program_address(&[b"creator-vault", create.payer.as_ref()], &PUMP_FUN_ID).0;

    let global_params = Pubkey::find_program_address(&[b"global-params"], &MAYHEM_PROGRAM_ID).0;
    let sol_vault = Pubkey::find_program_address(&[b"sol-vault"], &MAYHEM_PROGRAM_ID).0;
    let mayhem_state =
        Pubkey::find_program_address(&[b"mayhem-state", create.mint.as_ref()], &MAYHEM_PROGRAM_ID)
            .0;
    let mayhem_token_vault =
        derive_associated_token_address(&sol_vault, &create.mint, &TOKEN_2022_PROGRAM_ID);

    let mut create_data =
        Vec::with_capacity(8 + create.name.len() + create.symbol.len() + create.uri.len() + 32 + 2);
    create_data.extend_from_slice(&CREATE_V2_IX_DISCRIM);
    encode_borsh_string(&create.name, &mut create_data);
    encode_borsh_string(&create.symbol, &mut create_data);
    encode_borsh_string(&create.uri, &mut create_data);
    create_data.extend_from_slice(create.payer.as_ref()); // creator
    create_data.push(u8::from(create.is_mayhem_mode));
    create_data.push(0); // is_cashback_enabled

    let create_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(create.mint, true),
            AccountMeta::new_readonly(mint_authority, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new(MAYHEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(global_params, false),
            AccountMeta::new(sol_vault, false),
            AccountMeta::new(mayhem_state, false),
            AccountMeta::new(mayhem_token_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: create_data,
    };

    let extend_account_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: EXTEND_ACCOUNT_IX_DISCRIM.to_vec(),
    };

    let create_ata_ix = create_associated_token_account_idempotent(
        &create.payer,
        &create.payer,
        &create.mint,
        &TOKEN_2022_PROGRAM_ID,
    );

    let mut buy_data = Vec::with_capacity(8 + 8 + 8 + 1);
    buy_data.extend_from_slice(&BUY_DISCRIM);
    buy_data.extend_from_slice(&buy.amount.to_le_bytes());
    buy_data.extend_from_slice(&buy.max_sol_cost.to_le_bytes());
    buy_data.push(u8::from(buy.track_volume));

    let buy_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(buy.fee_recipient, false),
            AccountMeta::new_readonly(create.mint, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new(associated_user, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new(creator_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
            AccountMeta::new(GLOBAL_VOLUME_ACCUMULATOR, false),
            AccountMeta::new(user_volume_accumulator, false),
            AccountMeta::new_readonly(FEE_CONFIG, false),
            AccountMeta::new_readonly(FEE_PROGRAM, false),
            AccountMeta::new_readonly(bonding_curve_v2, false),
        ],
        data: buy_data,
    };

    let mut instructions = vec![create_ix, extend_account_ix, create_ata_ix, buy_ix];
    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedCreateTx {
        payer: create.payer,
        required_signers: vec![create.payer, create.mint],
        derived: DerivedAddresses::new()
            .insert("global", global)
            .insert("mint_authority", mint_authority)
            .insert("bonding_curve", bonding_curve)
            .insert("associated_bonding_curve", associated_bonding_curve)
            .insert("event_authority", event_authority)
            .insert("bonding_curve_v2", bonding_curve_v2)
            .insert("associated_user", associated_user)
            .insert("creator_vault", creator_vault)
            .insert("global_volume_accumulator", GLOBAL_VOLUME_ACCUMULATOR)
            .insert("user_volume_accumulator", user_volume_accumulator)
            .insert("fee_recipient", buy.fee_recipient)
            .insert("fee_config", FEE_CONFIG)
            .insert("fee_program", FEE_PROGRAM)
            .insert("mayhem_program_id", MAYHEM_PROGRAM_ID)
            .insert("mayhem_global_params", global_params)
            .insert("mayhem_sol_vault", sol_vault)
            .insert("mayhem_state", mayhem_state)
            .insert("mayhem_token_vault", mayhem_token_vault),
        priority_fee_addresses: vec![
            PUMP_FUN_ID,
            TOKEN_2022_PROGRAM_ID,
            ATA_PROGRAM_ID,
            FEE_PROGRAM,
            buy.fee_recipient,
            MAYHEM_PROGRAM_ID,
        ],
        instructions,
    })
}

pub fn plan_pump_fun_create_v2_and_buy_exact_sol_in(
    create: PumpFunCreatePlan,
    buy: PumpFunAutoBuyExactSolInPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedCreateTx> {
    let global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
    let mint_authority = Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
    let bonding_curve =
        Pubkey::find_program_address(&[b"bonding-curve", create.mint.as_ref()], &PUMP_FUN_ID).0;
    let associated_bonding_curve =
        derive_associated_token_address(&bonding_curve, &create.mint, &TOKEN_2022_PROGRAM_ID);
    let event_authority = Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;
    let bonding_curve_v2 =
        Pubkey::find_program_address(&[b"bonding-curve-v2", create.mint.as_ref()], &PUMP_FUN_ID).0;

    let associated_user =
        derive_associated_token_address(&create.payer, &create.mint, &TOKEN_2022_PROGRAM_ID);
    let user_volume_accumulator = Pubkey::find_program_address(
        &[b"user_volume_accumulator", create.payer.as_ref()],
        &PUMP_FUN_ID,
    )
    .0;
    let creator_vault =
        Pubkey::find_program_address(&[b"creator-vault", create.payer.as_ref()], &PUMP_FUN_ID).0;

    let global_params = Pubkey::find_program_address(&[b"global-params"], &MAYHEM_PROGRAM_ID).0;
    let sol_vault = Pubkey::find_program_address(&[b"sol-vault"], &MAYHEM_PROGRAM_ID).0;
    let mayhem_state =
        Pubkey::find_program_address(&[b"mayhem-state", create.mint.as_ref()], &MAYHEM_PROGRAM_ID)
            .0;
    let mayhem_token_vault =
        derive_associated_token_address(&sol_vault, &create.mint, &TOKEN_2022_PROGRAM_ID);

    let mut create_data =
        Vec::with_capacity(8 + create.name.len() + create.symbol.len() + create.uri.len() + 32 + 2);
    create_data.extend_from_slice(&CREATE_V2_IX_DISCRIM);
    encode_borsh_string(&create.name, &mut create_data);
    encode_borsh_string(&create.symbol, &mut create_data);
    encode_borsh_string(&create.uri, &mut create_data);
    create_data.extend_from_slice(create.payer.as_ref()); // creator
    create_data.push(u8::from(create.is_mayhem_mode));
    create_data.push(0); // is_cashback_enabled

    let create_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(create.mint, true),
            AccountMeta::new_readonly(mint_authority, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new(MAYHEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(global_params, false),
            AccountMeta::new(sol_vault, false),
            AccountMeta::new(mayhem_state, false),
            AccountMeta::new(mayhem_token_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: create_data,
    };

    let extend_account_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
        ],
        data: EXTEND_ACCOUNT_IX_DISCRIM.to_vec(),
    };

    let create_ata_ix = create_associated_token_account_idempotent(
        &create.payer,
        &create.payer,
        &create.mint,
        &TOKEN_2022_PROGRAM_ID,
    );

    let mut buy_data = Vec::with_capacity(8 + 8 + 8 + 1);
    buy_data.extend_from_slice(&BUY_EXACT_SOL_IN_DISCRIM);
    buy_data.extend_from_slice(&buy.spendable_sol_in.to_le_bytes());
    buy_data.extend_from_slice(&buy.min_tokens_out.to_le_bytes());
    buy_data.push(u8::from(buy.track_volume));

    let buy_ix = Instruction {
        program_id: PUMP_FUN_ID,
        accounts: vec![
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(buy.fee_recipient, false),
            AccountMeta::new_readonly(create.mint, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(associated_bonding_curve, false),
            AccountMeta::new(associated_user, false),
            AccountMeta::new(create.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new(creator_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_ID, false),
            AccountMeta::new(GLOBAL_VOLUME_ACCUMULATOR, false),
            AccountMeta::new(user_volume_accumulator, false),
            AccountMeta::new_readonly(FEE_CONFIG, false),
            AccountMeta::new_readonly(FEE_PROGRAM, false),
            AccountMeta::new_readonly(bonding_curve_v2, false),
        ],
        data: buy_data,
    };

    let mut instructions = vec![create_ix, extend_account_ix, create_ata_ix, buy_ix];
    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedCreateTx {
        payer: create.payer,
        required_signers: vec![create.payer, create.mint],
        derived: DerivedAddresses::new()
            .insert("global", global)
            .insert("mint_authority", mint_authority)
            .insert("bonding_curve", bonding_curve)
            .insert("associated_bonding_curve", associated_bonding_curve)
            .insert("event_authority", event_authority)
            .insert("bonding_curve_v2", bonding_curve_v2)
            .insert("associated_user", associated_user)
            .insert("creator_vault", creator_vault)
            .insert("global_volume_accumulator", GLOBAL_VOLUME_ACCUMULATOR)
            .insert("user_volume_accumulator", user_volume_accumulator)
            .insert("fee_recipient", buy.fee_recipient)
            .insert("fee_config", FEE_CONFIG)
            .insert("fee_program", FEE_PROGRAM)
            .insert("mayhem_program_id", MAYHEM_PROGRAM_ID)
            .insert("mayhem_global_params", global_params)
            .insert("mayhem_sol_vault", sol_vault)
            .insert("mayhem_state", mayhem_state)
            .insert("mayhem_token_vault", mayhem_token_vault),
        priority_fee_addresses: vec![
            PUMP_FUN_ID,
            TOKEN_2022_PROGRAM_ID,
            ATA_PROGRAM_ID,
            FEE_PROGRAM,
            buy.fee_recipient,
            MAYHEM_PROGRAM_ID,
        ],
        instructions,
    })
}

pub fn plan_raydium_launchpad_create(
    plan: RaydiumLaunchpadCreatePlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedCreateTx> {
    fn to_anchor_pubkey(key: Pubkey) -> anchor_lang::prelude::Pubkey {
        anchor_lang::prelude::Pubkey::new_from_array(key.to_bytes())
    }

    fn to_solana_account_metas(metas: Vec<anchor_lang::prelude::AccountMeta>) -> Vec<AccountMeta> {
        metas
            .into_iter()
            .map(|meta| AccountMeta {
                pubkey: Pubkey::from(meta.pubkey.to_bytes()),
                is_signer: meta.is_signer,
                is_writable: meta.is_writable,
            })
            .collect()
    }

    let program_id = plan.launchpad_program_id;
    let authority = Pubkey::find_program_address(&[LAUNCHPAD_AUTH_SEED], &program_id).0;
    let pool_state = Pubkey::find_program_address(
        &[
            RAYDIUM_LAUNCHPAD_POOL_SEED,
            plan.base_mint.as_ref(),
            plan.quote_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let base_vault = Pubkey::find_program_address(
        &[
            RAYDIUM_LAUNCHPAD_POOL_VAULT_SEED,
            pool_state.as_ref(),
            plan.base_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let quote_vault = Pubkey::find_program_address(
        &[
            RAYDIUM_LAUNCHPAD_POOL_VAULT_SEED,
            pool_state.as_ref(),
            plan.quote_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let event_authority = Pubkey::find_program_address(&[LAUNCHPAD_EVENT_AUTH_SEED], &program_id).0;

    let metadata_account = match plan.base_token_program {
        RaydiumLaunchpadBaseTokenProgram::Token => {
            Some(derive_metaplex_metadata_pda(&plan.base_mint))
        }
        RaydiumLaunchpadBaseTokenProgram::Token2022 => None,
    };

    let mint_param = || raydium_launchpad_idl::MintParams {
        decimals: plan.decimals,
        name: plan.name.clone(),
        symbol: plan.symbol.clone(),
        uri: plan.uri.clone(),
    };
    let curve_param = || match &plan.curve {
        RaydiumLaunchpadCurveParams::Constant {
            supply,
            total_base_sell,
            total_quote_fund_raising,
            migrate_type,
        } => raydium_launchpad_idl::CurveParams::Constant {
            data: raydium_launchpad_idl::ConstantCurve {
                supply: *supply,
                total_base_sell: *total_base_sell,
                total_quote_fund_raising: *total_quote_fund_raising,
                migrate_type: *migrate_type,
            },
        },
        RaydiumLaunchpadCurveParams::Fixed {
            supply,
            total_quote_fund_raising,
            migrate_type,
        } => raydium_launchpad_idl::CurveParams::Fixed {
            data: raydium_launchpad_idl::FixedCurve {
                supply: *supply,
                total_quote_fund_raising: *total_quote_fund_raising,
                migrate_type: *migrate_type,
            },
        },
        RaydiumLaunchpadCurveParams::Linear {
            supply,
            total_quote_fund_raising,
            migrate_type,
        } => raydium_launchpad_idl::CurveParams::Linear {
            data: raydium_launchpad_idl::LinearCurve {
                supply: *supply,
                total_quote_fund_raising: *total_quote_fund_raising,
                migrate_type: *migrate_type,
            },
        },
    };
    let vesting_param = || raydium_launchpad_idl::VestingParams {
        total_locked_amount: plan.vesting.total_locked_amount,
        cliff_period: plan.vesting.cliff_period,
        unlock_period: plan.vesting.unlock_period,
    };
    let amm_fee_on_param = || match plan.amm_fee_on {
        RaydiumLaunchpadAmmFeeOn::QuoteToken => raydium_launchpad_idl::AmmFeeOn::QuoteToken,
        RaydiumLaunchpadAmmFeeOn::BothToken => raydium_launchpad_idl::AmmFeeOn::BothToken,
    };

    let mut priority_fee_addresses = vec![program_id];
    let ix = match plan.base_token_program {
        RaydiumLaunchpadBaseTokenProgram::Token => {
            let metadata_account = metadata_account
                .context("raydium launchpad tokenkeg create missing metaplex metadata PDA")?;
            priority_fee_addresses.push(TOKEN_PROGRAM_ID);
            priority_fee_addresses.push(METADATA_PROGRAM_ID);

            let accounts = raydium_launchpad_idl::accounts::InitializeV2 {
                payer: to_anchor_pubkey(plan.payer),
                creator: to_anchor_pubkey(plan.creator),
                global_config: to_anchor_pubkey(plan.global_config),
                platform_config: to_anchor_pubkey(plan.platform_config),
                authority: to_anchor_pubkey(authority),
                pool_state: to_anchor_pubkey(pool_state),
                base_mint: to_anchor_pubkey(plan.base_mint),
                quote_mint: to_anchor_pubkey(plan.quote_mint),
                base_vault: to_anchor_pubkey(base_vault),
                quote_vault: to_anchor_pubkey(quote_vault),
                metadata_account: to_anchor_pubkey(metadata_account),
                base_token_program: to_anchor_pubkey(TOKEN_PROGRAM_ID),
                quote_token_program: to_anchor_pubkey(TOKEN_PROGRAM_ID),
                metadata_program: to_anchor_pubkey(METADATA_PROGRAM_ID),
                system_program: to_anchor_pubkey(SYSTEM_PROGRAM),
                rent_program: to_anchor_pubkey(RENT_SYSVAR_ID),
                event_authority: to_anchor_pubkey(event_authority),
                program: to_anchor_pubkey(program_id),
            };
            let ix_data = raydium_launchpad_idl::instruction::InitializeV2 {
                _base_mint_param: mint_param(),
                _curve_param: curve_param(),
                _vesting_param: vesting_param(),
                _amm_fee_on: amm_fee_on_param(),
            };
            Instruction {
                program_id,
                accounts: to_solana_account_metas(accounts.to_account_metas(None)),
                data: ix_data.data(),
            }
        }
        RaydiumLaunchpadBaseTokenProgram::Token2022 => {
            priority_fee_addresses.push(TOKEN_2022_PROGRAM_ID);
            priority_fee_addresses.push(TOKEN_PROGRAM_ID);

            let transfer_fee_extension_param = plan.transfer_fee_extension.as_ref().map(|params| {
                raydium_launchpad_idl::TransferFeeExtensionParams {
                    transfer_fee_basis_points: params.transfer_fee_basis_points,
                    maximum_fee: params.maximum_fee,
                }
            });
            let accounts = raydium_launchpad_idl::accounts::InitializeWithToken2022 {
                payer: to_anchor_pubkey(plan.payer),
                creator: to_anchor_pubkey(plan.creator),
                global_config: to_anchor_pubkey(plan.global_config),
                platform_config: to_anchor_pubkey(plan.platform_config),
                authority: to_anchor_pubkey(authority),
                pool_state: to_anchor_pubkey(pool_state),
                base_mint: to_anchor_pubkey(plan.base_mint),
                quote_mint: to_anchor_pubkey(plan.quote_mint),
                base_vault: to_anchor_pubkey(base_vault),
                quote_vault: to_anchor_pubkey(quote_vault),
                base_token_program: to_anchor_pubkey(TOKEN_2022_PROGRAM_ID),
                quote_token_program: to_anchor_pubkey(TOKEN_PROGRAM_ID),
                system_program: to_anchor_pubkey(SYSTEM_PROGRAM),
                event_authority: to_anchor_pubkey(event_authority),
                program: to_anchor_pubkey(program_id),
            };
            let ix_data = raydium_launchpad_idl::instruction::InitializeWithToken2022 {
                _base_mint_param: mint_param(),
                _curve_param: curve_param(),
                _vesting_param: vesting_param(),
                _amm_fee_on: amm_fee_on_param(),
                _transfer_fee_extension_param: transfer_fee_extension_param,
            };
            Instruction {
                program_id,
                accounts: to_solana_account_metas(accounts.to_account_metas(None)),
                data: ix_data.data(),
            }
        }
    };

    let mut instructions = vec![ix];
    compute_budget.prepend_instructions(&mut instructions);

    let mut derived = DerivedAddresses::new()
        .insert("authority", authority)
        .insert("pool_state", pool_state)
        .insert("base_vault", base_vault)
        .insert("quote_vault", quote_vault)
        .insert("event_authority", event_authority)
        .insert("quote_mint", plan.quote_mint);
    if let Some(metadata_account) = metadata_account {
        derived = derived.insert("metadata", metadata_account);
    }

    Ok(PlannedCreateTx {
        payer: plan.payer,
        required_signers: vec![plan.payer, plan.base_mint],
        derived,
        priority_fee_addresses,
        instructions,
    })
}

pub fn plan_raydium_launchpad_create_and_buy_exact_sol_in(
    create: RaydiumLaunchpadCreatePlan,
    buy: RaydiumLaunchpadAutoBuyExactSolInPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedCreateTx> {
    anyhow::ensure!(
        buy.amount_in_quote_lamports > 0,
        "raydium launchpad auto-buy amount_in_quote_lamports must be > 0"
    );
    anyhow::ensure!(
        buy.min_base_amount_out > 0,
        "raydium launchpad auto-buy min_base_amount_out must be > 0"
    );
    anyhow::ensure!(
        buy.share_fee_rate == 0,
        "raydium launchpad auto-buy share_fee_rate is not supported (must be 0)"
    );

    let program_id = create.launchpad_program_id;
    let authority = Pubkey::find_program_address(&[LAUNCHPAD_AUTH_SEED], &program_id).0;
    let pool_state = Pubkey::find_program_address(
        &[
            RAYDIUM_LAUNCHPAD_POOL_SEED,
            create.base_mint.as_ref(),
            create.quote_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let base_vault = Pubkey::find_program_address(
        &[
            RAYDIUM_LAUNCHPAD_POOL_VAULT_SEED,
            pool_state.as_ref(),
            create.base_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let quote_vault = Pubkey::find_program_address(
        &[
            RAYDIUM_LAUNCHPAD_POOL_VAULT_SEED,
            pool_state.as_ref(),
            create.quote_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let event_authority = Pubkey::find_program_address(&[LAUNCHPAD_EVENT_AUTH_SEED], &program_id).0;

    let base_token_program_id = match create.base_token_program {
        RaydiumLaunchpadBaseTokenProgram::Token => TOKEN_PROGRAM_ID,
        RaydiumLaunchpadBaseTokenProgram::Token2022 => TOKEN_2022_PROGRAM_ID,
    };
    let quote_token_program_id = TOKEN_PROGRAM_ID;

    let user_base_ata =
        derive_associated_token_address(&create.payer, &create.base_mint, &base_token_program_id);
    let user_quote_ata =
        derive_associated_token_address(&create.payer, &create.quote_mint, &quote_token_program_id);

    let platform_fee_vault = Pubkey::find_program_address(
        &[create.platform_config.as_ref(), create.quote_mint.as_ref()],
        &program_id,
    )
    .0;
    let creator_fee_vault = Pubkey::find_program_address(
        &[create.creator.as_ref(), create.quote_mint.as_ref()],
        &program_id,
    )
    .0;

    let mut planned = plan_raydium_launchpad_create(create.clone(), compute_budget)?;

    planned
        .derived
        .map
        .insert("user_base_ata".to_string(), user_base_ata);
    planned
        .derived
        .map
        .insert("user_quote_ata".to_string(), user_quote_ata);
    planned
        .derived
        .map
        .insert("platform_fee_vault".to_string(), platform_fee_vault);
    planned
        .derived
        .map
        .insert("creator_fee_vault".to_string(), creator_fee_vault);

    planned.priority_fee_addresses.push(ATA_PROGRAM_ID);
    planned.priority_fee_addresses.push(SYSTEM_PROGRAM);
    planned.priority_fee_addresses.push(platform_fee_vault);
    planned.priority_fee_addresses.push(creator_fee_vault);

    planned
        .instructions
        .push(create_associated_token_account_idempotent(
            &create.payer,
            &create.payer,
            &create.base_mint,
            &base_token_program_id,
        ));
    planned
        .instructions
        .push(create_associated_token_account_idempotent(
            &create.payer,
            &create.payer,
            &create.quote_mint,
            &quote_token_program_id,
        ));
    planned.instructions.push(system_instruction_if::transfer(
        &create.payer,
        &user_quote_ata,
        buy.amount_in_quote_lamports,
    ));
    planned
        .instructions
        .push(sync_native(&quote_token_program_id, &user_quote_ata)?);

    let mut buy_data = Vec::with_capacity(8 + 8 + 8 + 8);
    buy_data.extend_from_slice(&crate::dex::raydium_launchpad::BUY_EXACT_IN_IX_DISCRIM);
    buy_data.extend_from_slice(&buy.amount_in_quote_lamports.to_le_bytes());
    buy_data.extend_from_slice(&buy.min_base_amount_out.to_le_bytes());
    buy_data.extend_from_slice(&buy.share_fee_rate.to_le_bytes());

    planned.instructions.push(Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(create.payer, true),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new_readonly(create.global_config, false),
            AccountMeta::new_readonly(create.platform_config, false),
            AccountMeta::new(pool_state, false),
            AccountMeta::new(user_base_ata, false),
            AccountMeta::new(user_quote_ata, false),
            AccountMeta::new(base_vault, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new_readonly(create.base_mint, false),
            AccountMeta::new_readonly(create.quote_mint, false),
            AccountMeta::new_readonly(base_token_program_id, false),
            AccountMeta::new_readonly(quote_token_program_id, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(program_id, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new(platform_fee_vault, false),
            AccountMeta::new(creator_fee_vault, false),
        ],
        data: buy_data,
    });

    Ok(planned)
}

fn encode_borsh_string(value: &str, out: &mut Vec<u8>) {
    let len = value.len().min(u32::MAX as usize) as u32;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

#[allow(clippy::too_many_arguments)]
fn build_mpl_create_metadata_account_v3_ix(
    metadata_pda: Pubkey,
    mint: Pubkey,
    mint_authority: Pubkey,
    payer: Pubkey,
    update_authority: Pubkey,
    name: &str,
    symbol: &str,
    uri: &str,
    is_mutable: bool,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 256);
    data.push(MPL_CREATE_METADATA_ACCOUNT_V3_DISCRIM);
    encode_borsh_string(name, &mut data);
    encode_borsh_string(symbol, &mut data);
    encode_borsh_string(uri, &mut data);
    data.extend_from_slice(&0u16.to_le_bytes()); // seller_fee_basis_points
    data.push(0u8); // creators: None
    data.push(0u8); // collection: None
    data.push(0u8); // uses: None
    data.push(u8::from(is_mutable));
    data.push(0u8); // collection_details: None

    Instruction {
        program_id: METADATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(metadata_pda, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(mint_authority, true),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(update_authority, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
        ],
        data,
    }
}

pub fn plan_spl_token_create(
    plan: SplTokenCreatePlan,
    compute_budget: ComputeBudgetPlan,
    mint_rent_lamports: u64,
) -> anyhow::Result<PlannedCreateTx> {
    if plan.revoke_freeze_authority && !plan.freeze_authority {
        anyhow::bail!("revoke_freeze_authority=true requires freeze_authority=true");
    }

    let payer_ata = derive_associated_token_address(&plan.payer, &plan.mint, &TOKEN_PROGRAM_ID);
    let metadata_pda = derive_metaplex_metadata_pda(&plan.mint);

    let mut instructions = Vec::with_capacity(8);
    instructions.push(system_instruction_if::create_account(
        &plan.payer,
        &plan.mint,
        mint_rent_lamports,
        SplMint::LEN as u64,
        &TOKEN_PROGRAM_ID,
    ));

    instructions.push(
        spl_token::instruction::initialize_mint(
            &TOKEN_PROGRAM_ID,
            &plan.mint,
            &plan.payer,
            if plan.freeze_authority {
                Some(&plan.payer)
            } else {
                None
            },
            plan.decimals,
        )
        .map_err(|e| anyhow::anyhow!("failed to build SPL Token initialize_mint: {e:?}"))?,
    );

    instructions.push(create_associated_token_account(
        &plan.payer,
        &plan.payer,
        &plan.mint,
        &TOKEN_PROGRAM_ID,
    ));

    if plan.initial_supply > 0 {
        instructions.push(
            spl_token::instruction::mint_to(
                &TOKEN_PROGRAM_ID,
                &plan.mint,
                &payer_ata,
                &plan.payer,
                &[],
                plan.initial_supply,
            )
            .map_err(|e| anyhow::anyhow!("failed to build SPL Token mint_to: {e:?}"))?,
        );
    }

    instructions.push(build_mpl_create_metadata_account_v3_ix(
        metadata_pda,
        plan.mint,
        plan.payer,
        plan.payer,
        plan.payer,
        &plan.name,
        &plan.symbol,
        &plan.uri,
        plan.metadata_is_mutable,
    ));

    if plan.revoke_mint_authority {
        instructions.push(spl_token_set_authority_ix(
            &plan.mint,
            &plan.payer,
            spl_token::instruction::AuthorityType::MintTokens,
        )?);
    }

    if plan.revoke_freeze_authority {
        instructions.push(spl_token_set_authority_ix(
            &plan.mint,
            &plan.payer,
            spl_token::instruction::AuthorityType::FreezeAccount,
        )?);
    }

    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedCreateTx {
        payer: plan.payer,
        required_signers: vec![plan.payer, plan.mint],
        derived: DerivedAddresses::new()
            .insert("payer_ata", payer_ata)
            .insert("metadata", metadata_pda),
        priority_fee_addresses: vec![TOKEN_PROGRAM_ID, METADATA_PROGRAM_ID],
        instructions,
    })
}

pub fn spl_token_2022_inline_metadata_required_space(
    payer: Pubkey,
    mint: Pubkey,
    name: &str,
    symbol: &str,
    uri: &str,
) -> anyhow::Result<usize> {
    let fixed =
        ExtensionType::try_calculate_account_len::<SplMint2022>(&[ExtensionType::MetadataPointer])
            .map_err(|e| {
                anyhow::anyhow!("failed to calculate Token-2022 mint fixed size: {e:?}")
            })?;

    let metadata = Token2022Metadata {
        mint,
        name: name.to_string(),
        symbol: symbol.to_string(),
        uri: uri.to_string(),
        additional_metadata: vec![],
        ..Token2022Metadata::default()
    };
    let metadata_tlv = metadata
        .tlv_size_of()
        .map_err(|e| anyhow::anyhow!("failed to calculate token-2022 metadata tlv size: {e:?}"))?;

    let total = fixed.saturating_add(metadata_tlv);
    let adjusted = if total == SplMultisig::LEN {
        total + 1
    } else {
        total
    };
    // Silence unused warnings / preserve future intent (payer affects no sizing today).
    let _ = payer;
    Ok(adjusted)
}

pub fn plan_spl_token_2022_create(
    plan: SplToken2022CreatePlan,
    compute_budget: ComputeBudgetPlan,
    mint_rent_lamports: u64,
) -> anyhow::Result<PlannedCreateTx> {
    if plan.revoke_freeze_authority && !plan.freeze_authority {
        anyhow::bail!("revoke_freeze_authority=true requires freeze_authority=true");
    }

    let payer_ata =
        derive_associated_token_address(&plan.payer, &plan.mint, &TOKEN_2022_PROGRAM_ID);
    let mint_space =
        ExtensionType::try_calculate_account_len::<SplMint2022>(&[ExtensionType::MetadataPointer])
            .map_err(|e| {
                anyhow::anyhow!("failed to calculate Token-2022 mint fixed size: {e:?}")
            })?;

    let mut instructions = Vec::with_capacity(10);
    instructions.push(system_instruction_if::create_account(
        &plan.payer,
        &plan.mint,
        mint_rent_lamports,
        mint_space as u64,
        &TOKEN_2022_PROGRAM_ID,
    ));

    instructions.push(
        spl_token_2022::extension::metadata_pointer::instruction::initialize(
            &TOKEN_2022_PROGRAM_ID,
            &plan.mint,
            Some(plan.payer),
            Some(plan.mint),
        )
        .map_err(|e| anyhow::anyhow!("failed to build metadata pointer init ix: {e:?}"))?,
    );

    instructions.push(
        spl_token_2022::instruction::initialize_mint2(
            &TOKEN_2022_PROGRAM_ID,
            &plan.mint,
            &plan.payer,
            if plan.freeze_authority {
                Some(&plan.payer)
            } else {
                None
            },
            plan.decimals,
        )
        .map_err(|e| anyhow::anyhow!("failed to build token-2022 initialize_mint2: {e:?}"))?,
    );

    instructions.push(spl_token_metadata_interface::instruction::initialize(
        &TOKEN_2022_PROGRAM_ID,
        &plan.mint,
        &plan.payer,
        &plan.mint,
        &plan.payer,
        plan.name,
        plan.symbol,
        plan.uri,
    ));

    instructions.push(create_associated_token_account(
        &plan.payer,
        &plan.payer,
        &plan.mint,
        &TOKEN_2022_PROGRAM_ID,
    ));

    if plan.initial_supply > 0 {
        instructions.push(
            spl_token_2022::instruction::mint_to(
                &TOKEN_2022_PROGRAM_ID,
                &plan.mint,
                &payer_ata,
                &plan.payer,
                &[],
                plan.initial_supply,
            )
            .map_err(|e| anyhow::anyhow!("failed to build token-2022 mint_to: {e:?}"))?,
        );
    }

    if plan.revoke_mint_authority {
        instructions.push(spl_token_2022_set_authority_ix(
            &plan.mint,
            &plan.payer,
            spl_token_2022::instruction::AuthorityType::MintTokens,
        )?);
    }

    if plan.revoke_freeze_authority {
        instructions.push(spl_token_2022_set_authority_ix(
            &plan.mint,
            &plan.payer,
            spl_token_2022::instruction::AuthorityType::FreezeAccount,
        )?);
    }

    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedCreateTx {
        payer: plan.payer,
        required_signers: vec![plan.payer, plan.mint],
        derived: DerivedAddresses::new()
            .insert("payer_ata", payer_ata)
            .insert("metadata", plan.mint),
        priority_fee_addresses: vec![TOKEN_2022_PROGRAM_ID],
        instructions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex::pump_fun::{BUY_DISCRIM, BUY_EXACT_SOL_IN_DISCRIM};
    use solana_program::hash::hash;

    fn anchor_discriminator(namespace: &str, name: &str) -> [u8; 8] {
        let preimage = format!("{namespace}:{name}");
        let digest = hash(preimage.as_bytes()).to_bytes();
        let mut out = [0u8; 8];
        out.copy_from_slice(&digest[..8]);
        out
    }

    fn borsh_string(s: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + s.len());
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
        out
    }

    #[test]
    fn test_create_instruction_discriminators_match_anchor_layout() {
        assert_eq!(
            PUMP_FUN_CREATE_IX_DISCRIM,
            anchor_discriminator("global", "create")
        );
        assert_eq!(
            CREATE_V2_IX_DISCRIM,
            anchor_discriminator("global", "create_v2")
        );
        assert_eq!(BUY_DISCRIM, anchor_discriminator("global", "buy"));
        assert_eq!(
            EXTEND_ACCOUNT_IX_DISCRIM,
            anchor_discriminator("global", "extend_account")
        );
        assert_eq!(
            BUY_EXACT_SOL_IN_DISCRIM,
            anchor_discriminator("global", "buy_exact_sol_in")
        );
        assert_eq!(
            RAYDIUM_LAUNCHPAD_INITIALIZE_V2_IX_DISCRIM,
            anchor_discriminator("global", "initialize_v2")
        );
        assert_eq!(
            RAYDIUM_LAUNCHPAD_INITIALIZE_WITH_TOKEN_2022_IX_DISCRIM,
            anchor_discriminator("global", "initialize_with_token_2022")
        );
        assert_eq!(MPL_CREATE_METADATA_ACCOUNT_V3_DISCRIM, 33);
    }

    #[test]
    fn test_plan_pump_fun_create_derives_addresses_and_instruction_data() {
        let payer = Pubkey::new_from_array([1u8; 32]);
        let mint = Pubkey::new_from_array([2u8; 32]);
        let name = "Test Token";
        let symbol = "TST";
        let uri = "https://example.com/meta.json";

        let planned = plan_pump_fun_create(
            PumpFunCreatePlan {
                payer,
                mint,
                name: name.to_string(),
                symbol: symbol.to_string(),
                uri: uri.to_string(),
                is_mayhem_mode: false,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("pump fun create plan must succeed");

        assert_eq!(planned.payer, payer);
        assert_eq!(planned.required_signers, vec![payer, mint]);
        assert_eq!(planned.instructions.len(), 1);

        let expected_global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
        let expected_mint_authority =
            Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
        let expected_bonding_curve =
            Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &PUMP_FUN_ID).0;
        let expected_associated_bonding_curve =
            derive_associated_token_address(&expected_bonding_curve, &mint, &TOKEN_PROGRAM_ID);
        let expected_metadata = derive_metaplex_metadata_pda(&mint);
        let expected_event_authority =
            Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;

        assert_eq!(planned.derived.map.get("global"), Some(&expected_global));
        assert_eq!(
            planned.derived.map.get("mint_authority"),
            Some(&expected_mint_authority)
        );
        assert_eq!(
            planned.derived.map.get("bonding_curve"),
            Some(&expected_bonding_curve)
        );
        assert_eq!(
            planned.derived.map.get("associated_bonding_curve"),
            Some(&expected_associated_bonding_curve)
        );
        assert_eq!(
            planned.derived.map.get("metadata"),
            Some(&expected_metadata)
        );
        assert_eq!(
            planned.derived.map.get("event_authority"),
            Some(&expected_event_authority)
        );

        let ix = &planned.instructions[0];
        assert_eq!(ix.program_id, PUMP_FUN_ID);
        assert_eq!(ix.accounts[0].pubkey, mint);
        assert!(ix.accounts[0].is_signer);
        assert!(ix.accounts[0].is_writable);

        let mut expected_data = Vec::new();
        expected_data.extend_from_slice(&PUMP_FUN_CREATE_IX_DISCRIM);
        expected_data.extend_from_slice(&borsh_string(name));
        expected_data.extend_from_slice(&borsh_string(symbol));
        expected_data.extend_from_slice(&borsh_string(uri));
        expected_data.extend_from_slice(payer.as_ref());
        assert_eq!(ix.data, expected_data);
    }

    #[test]
    fn test_plan_pump_fun_create_and_buy_derives_addresses_and_instruction_data() {
        let payer = Pubkey::new_from_array([7u8; 32]);
        let mint = Pubkey::new_from_array([8u8; 32]);
        let name = "Test Token";
        let symbol = "TST";
        let uri = "https://example.com/meta.json";
        let fee_recipient = Pubkey::new_from_array([9u8; 32]);

        let planned = plan_pump_fun_create_and_buy(
            PumpFunCreatePlan {
                payer,
                mint,
                name: name.to_string(),
                symbol: symbol.to_string(),
                uri: uri.to_string(),
                is_mayhem_mode: false,
            },
            PumpFunAutoBuyPlan {
                amount: 123_456,
                max_sol_cost: 987_654_321,
                fee_recipient,
                track_volume: true,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("pump fun create+buy plan must succeed");

        assert_eq!(planned.payer, payer);
        assert_eq!(planned.required_signers, vec![payer, mint]);
        assert_eq!(planned.instructions.len(), 4);

        let expected_global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
        let expected_mint_authority =
            Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
        let expected_bonding_curve =
            Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &PUMP_FUN_ID).0;
        let expected_associated_bonding_curve =
            derive_associated_token_address(&expected_bonding_curve, &mint, &TOKEN_PROGRAM_ID);
        let expected_metadata = derive_metaplex_metadata_pda(&mint);
        let expected_event_authority =
            Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;
        let expected_associated_user =
            derive_associated_token_address(&payer, &mint, &TOKEN_PROGRAM_ID);
        let expected_user_volume_accumulator = Pubkey::find_program_address(
            &[b"user_volume_accumulator", payer.as_ref()],
            &PUMP_FUN_ID,
        )
        .0;
        let expected_creator_vault =
            Pubkey::find_program_address(&[b"creator-vault", payer.as_ref()], &PUMP_FUN_ID).0;
        let expected_bonding_curve_v2 =
            Pubkey::find_program_address(&[b"bonding-curve-v2", mint.as_ref()], &PUMP_FUN_ID).0;

        assert_eq!(planned.derived.map.get("global"), Some(&expected_global));
        assert_eq!(
            planned.derived.map.get("mint_authority"),
            Some(&expected_mint_authority)
        );
        assert_eq!(
            planned.derived.map.get("bonding_curve"),
            Some(&expected_bonding_curve)
        );
        assert_eq!(
            planned.derived.map.get("associated_bonding_curve"),
            Some(&expected_associated_bonding_curve)
        );
        assert_eq!(
            planned.derived.map.get("metadata"),
            Some(&expected_metadata)
        );
        assert_eq!(
            planned.derived.map.get("event_authority"),
            Some(&expected_event_authority)
        );
        assert_eq!(
            planned.derived.map.get("bonding_curve_v2"),
            Some(&expected_bonding_curve_v2)
        );
        assert_eq!(
            planned.derived.map.get("associated_user"),
            Some(&expected_associated_user)
        );
        assert_eq!(
            planned.derived.map.get("creator_vault"),
            Some(&expected_creator_vault)
        );
        assert_eq!(
            planned.derived.map.get("user_volume_accumulator"),
            Some(&expected_user_volume_accumulator)
        );

        let extend_ix = planned
            .instructions
            .get(1)
            .expect("extend_account instruction must exist");
        assert_eq!(extend_ix.program_id, PUMP_FUN_ID);
        assert_eq!(extend_ix.data, EXTEND_ACCOUNT_IX_DISCRIM.to_vec());
        assert_eq!(extend_ix.accounts.len(), 5);
        assert_eq!(extend_ix.accounts[0].pubkey, expected_bonding_curve);
        assert_eq!(extend_ix.accounts[1].pubkey, payer);
        assert!(extend_ix.accounts[1].is_signer);
        assert!(extend_ix.accounts[1].is_writable);

        let buy_ix = planned
            .instructions
            .last()
            .expect("buy instruction must exist");
        assert_eq!(buy_ix.program_id, PUMP_FUN_ID);
        assert_eq!(buy_ix.accounts.len(), 17);
        assert_eq!(buy_ix.accounts[0].pubkey, expected_global);
        assert_eq!(buy_ix.accounts[1].pubkey, fee_recipient);
        assert_eq!(buy_ix.accounts[2].pubkey, mint);
        assert_eq!(buy_ix.accounts[3].pubkey, expected_bonding_curve);
        assert_eq!(buy_ix.accounts[4].pubkey, expected_associated_bonding_curve);
        assert_eq!(buy_ix.accounts[5].pubkey, expected_associated_user);
        assert_eq!(buy_ix.accounts[6].pubkey, payer);
        assert!(buy_ix.accounts[6].is_signer);
        assert!(buy_ix.accounts[6].is_writable);
        assert_eq!(
            buy_ix
                .accounts
                .last()
                .expect("bonding_curve_v2 remaining account must exist")
                .pubkey,
            expected_bonding_curve_v2
        );

        let mut expected_data = Vec::new();
        expected_data.extend_from_slice(&BUY_DISCRIM);
        expected_data.extend_from_slice(&123_456u64.to_le_bytes());
        expected_data.extend_from_slice(&987_654_321u64.to_le_bytes());
        expected_data.push(1);
        assert_eq!(buy_ix.data, expected_data);
    }

    #[test]
    fn test_plan_pump_fun_create_v2_and_buy_derives_addresses_and_instruction_data() {
        let payer = Pubkey::new_from_array([10u8; 32]);
        let mint = Pubkey::new_from_array([11u8; 32]);
        let name = "Test Token";
        let symbol = "TST";
        let uri = "https://example.com/meta.json";
        let fee_recipient = Pubkey::new_from_array([12u8; 32]);

        let planned = plan_pump_fun_create_v2_and_buy(
            PumpFunCreatePlan {
                payer,
                mint,
                name: name.to_string(),
                symbol: symbol.to_string(),
                uri: uri.to_string(),
                is_mayhem_mode: false,
            },
            PumpFunAutoBuyPlan {
                amount: 123_456,
                max_sol_cost: 987_654_321,
                fee_recipient,
                track_volume: true,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("pump fun create_v2+buy plan must succeed");

        assert_eq!(planned.payer, payer);
        assert_eq!(planned.required_signers, vec![payer, mint]);
        assert_eq!(planned.instructions.len(), 4);

        let expected_global = Pubkey::find_program_address(&[b"global"], &PUMP_FUN_ID).0;
        let expected_mint_authority =
            Pubkey::find_program_address(&[b"mint-authority"], &PUMP_FUN_ID).0;
        let expected_bonding_curve =
            Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &PUMP_FUN_ID).0;
        let expected_associated_bonding_curve =
            derive_associated_token_address(&expected_bonding_curve, &mint, &TOKEN_2022_PROGRAM_ID);
        let expected_event_authority =
            Pubkey::find_program_address(&[b"__event_authority"], &PUMP_FUN_ID).0;
        let expected_associated_user =
            derive_associated_token_address(&payer, &mint, &TOKEN_2022_PROGRAM_ID);
        let expected_user_volume_accumulator = Pubkey::find_program_address(
            &[b"user_volume_accumulator", payer.as_ref()],
            &PUMP_FUN_ID,
        )
        .0;
        let expected_creator_vault =
            Pubkey::find_program_address(&[b"creator-vault", payer.as_ref()], &PUMP_FUN_ID).0;
        let expected_bonding_curve_v2 =
            Pubkey::find_program_address(&[b"bonding-curve-v2", mint.as_ref()], &PUMP_FUN_ID).0;

        let expected_mayhem_global_params =
            Pubkey::find_program_address(&[b"global-params"], &MAYHEM_PROGRAM_ID).0;
        let expected_mayhem_sol_vault =
            Pubkey::find_program_address(&[b"sol-vault"], &MAYHEM_PROGRAM_ID).0;
        let expected_mayhem_state =
            Pubkey::find_program_address(&[b"mayhem-state", mint.as_ref()], &MAYHEM_PROGRAM_ID).0;
        let expected_mayhem_token_vault = derive_associated_token_address(
            &expected_mayhem_sol_vault,
            &mint,
            &TOKEN_2022_PROGRAM_ID,
        );

        assert_eq!(planned.derived.map.get("global"), Some(&expected_global));
        assert_eq!(
            planned.derived.map.get("mint_authority"),
            Some(&expected_mint_authority)
        );
        assert_eq!(
            planned.derived.map.get("bonding_curve"),
            Some(&expected_bonding_curve)
        );
        assert_eq!(
            planned.derived.map.get("associated_bonding_curve"),
            Some(&expected_associated_bonding_curve)
        );
        assert_eq!(
            planned.derived.map.get("event_authority"),
            Some(&expected_event_authority)
        );
        assert_eq!(
            planned.derived.map.get("bonding_curve_v2"),
            Some(&expected_bonding_curve_v2)
        );
        assert_eq!(
            planned.derived.map.get("associated_user"),
            Some(&expected_associated_user)
        );
        assert_eq!(
            planned.derived.map.get("creator_vault"),
            Some(&expected_creator_vault)
        );
        assert_eq!(
            planned.derived.map.get("user_volume_accumulator"),
            Some(&expected_user_volume_accumulator)
        );
        assert_eq!(
            planned.derived.map.get("mayhem_global_params"),
            Some(&expected_mayhem_global_params)
        );
        assert_eq!(
            planned.derived.map.get("mayhem_sol_vault"),
            Some(&expected_mayhem_sol_vault)
        );
        assert_eq!(
            planned.derived.map.get("mayhem_state"),
            Some(&expected_mayhem_state)
        );
        assert_eq!(
            planned.derived.map.get("mayhem_token_vault"),
            Some(&expected_mayhem_token_vault)
        );

        let create_ix = planned
            .instructions
            .first()
            .expect("create_v2 instruction must exist");
        assert_eq!(create_ix.program_id, PUMP_FUN_ID);
        assert_eq!(create_ix.data[..8], CREATE_V2_IX_DISCRIM);
        assert_eq!(create_ix.data[create_ix.data.len() - 2], 0);
        assert_eq!(create_ix.data[create_ix.data.len() - 1], 0);
        assert_eq!(create_ix.accounts[0].pubkey, mint);
        assert_eq!(create_ix.accounts[7].pubkey, TOKEN_2022_PROGRAM_ID);
        assert_eq!(create_ix.accounts[9].pubkey, MAYHEM_PROGRAM_ID);

        let extend_ix = planned
            .instructions
            .get(1)
            .expect("extend_account instruction must exist");
        assert_eq!(extend_ix.program_id, PUMP_FUN_ID);
        assert_eq!(extend_ix.data, EXTEND_ACCOUNT_IX_DISCRIM.to_vec());

        let buy_ix = planned
            .instructions
            .last()
            .expect("buy instruction must exist");
        assert_eq!(buy_ix.program_id, PUMP_FUN_ID);
        assert_eq!(buy_ix.accounts.len(), 17);
        assert_eq!(buy_ix.accounts[8].pubkey, TOKEN_2022_PROGRAM_ID);
        assert_eq!(
            buy_ix
                .accounts
                .last()
                .expect("bonding_curve_v2 remaining account must exist")
                .pubkey,
            expected_bonding_curve_v2
        );
        assert_eq!(buy_ix.data[..8], BUY_DISCRIM);
    }

    #[test]
    fn test_plan_pump_fun_create_and_buy_exact_sol_in_derives_addresses_and_instruction_data() {
        let payer = Pubkey::new_from_array([13u8; 32]);
        let mint = Pubkey::new_from_array([14u8; 32]);
        let name = "Test Token";
        let symbol = "TST";
        let uri = "https://example.com/meta.json";
        let fee_recipient = Pubkey::new_from_array([15u8; 32]);

        let planned = plan_pump_fun_create_and_buy_exact_sol_in(
            PumpFunCreatePlan {
                payer,
                mint,
                name: name.to_string(),
                symbol: symbol.to_string(),
                uri: uri.to_string(),
                is_mayhem_mode: false,
            },
            PumpFunAutoBuyExactSolInPlan {
                spendable_sol_in: 1_000_001,
                min_tokens_out: 123_456,
                fee_recipient,
                track_volume: true,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("pump fun create+buy_exact_sol_in plan must succeed");

        assert_eq!(planned.instructions.len(), 4);

        let expected_bonding_curve =
            Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &PUMP_FUN_ID).0;
        let expected_bonding_curve_v2 =
            Pubkey::find_program_address(&[b"bonding-curve-v2", mint.as_ref()], &PUMP_FUN_ID).0;
        let extend_ix = planned
            .instructions
            .get(1)
            .expect("extend_account instruction must exist");
        assert_eq!(extend_ix.program_id, PUMP_FUN_ID);
        assert_eq!(extend_ix.data, EXTEND_ACCOUNT_IX_DISCRIM.to_vec());
        assert_eq!(extend_ix.accounts[0].pubkey, expected_bonding_curve);

        let buy_ix = planned
            .instructions
            .last()
            .expect("buy_exact_sol_in instruction must exist");
        assert_eq!(buy_ix.program_id, PUMP_FUN_ID);
        assert_eq!(buy_ix.accounts.len(), 17);
        assert_eq!(
            buy_ix
                .accounts
                .last()
                .expect("bonding_curve_v2 remaining account must exist")
                .pubkey,
            expected_bonding_curve_v2
        );
        assert_eq!(buy_ix.data[..8], BUY_EXACT_SOL_IN_DISCRIM);

        let mut expected_data = Vec::new();
        expected_data.extend_from_slice(&BUY_EXACT_SOL_IN_DISCRIM);
        expected_data.extend_from_slice(&1_000_001u64.to_le_bytes());
        expected_data.extend_from_slice(&123_456u64.to_le_bytes());
        expected_data.push(1);
        assert_eq!(buy_ix.data, expected_data);
    }

    #[test]
    fn test_plan_pump_fun_create_v2_and_buy_exact_sol_in_derives_addresses_and_instruction_data() {
        let payer = Pubkey::new_from_array([16u8; 32]);
        let mint = Pubkey::new_from_array([17u8; 32]);
        let name = "Test Token";
        let symbol = "TST";
        let uri = "https://example.com/meta.json";
        let fee_recipient = Pubkey::new_from_array([18u8; 32]);

        let planned = plan_pump_fun_create_v2_and_buy_exact_sol_in(
            PumpFunCreatePlan {
                payer,
                mint,
                name: name.to_string(),
                symbol: symbol.to_string(),
                uri: uri.to_string(),
                is_mayhem_mode: false,
            },
            PumpFunAutoBuyExactSolInPlan {
                spendable_sol_in: 1_000_001,
                min_tokens_out: 123_456,
                fee_recipient,
                track_volume: true,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("pump fun create_v2+buy_exact_sol_in plan must succeed");

        let buy_ix = planned
            .instructions
            .last()
            .expect("buy_exact_sol_in instruction must exist");
        assert_eq!(buy_ix.program_id, PUMP_FUN_ID);
        assert_eq!(buy_ix.accounts.len(), 17);
        assert_eq!(buy_ix.accounts[8].pubkey, TOKEN_2022_PROGRAM_ID);
        assert_eq!(buy_ix.data[..8], BUY_EXACT_SOL_IN_DISCRIM);
    }

    #[test]
    fn test_plan_spl_token_create_metadata_ix_is_deterministic() {
        let payer = Pubkey::new_from_array([3u8; 32]);
        let mint = Pubkey::new_from_array([4u8; 32]);

        let planned = plan_spl_token_create(
            SplTokenCreatePlan {
                payer,
                mint,
                name: "Example".to_string(),
                symbol: "EX".to_string(),
                uri: "https://example.com/token.json".to_string(),
                decimals: 6,
                initial_supply: 0,
                freeze_authority: false,
                revoke_mint_authority: false,
                revoke_freeze_authority: false,
                metadata_is_mutable: true,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
            123,
        )
        .expect("spl token create plan must succeed");

        assert_eq!(planned.required_signers, vec![payer, mint]);
        assert_eq!(
            planned.derived.map.get("payer_ata").copied(),
            Some(derive_associated_token_address(
                &payer,
                &mint,
                &TOKEN_PROGRAM_ID
            ))
        );
        assert_eq!(
            planned.derived.map.get("metadata").copied(),
            Some(derive_metaplex_metadata_pda(&mint))
        );

        // create_account, initialize_mint, create_ata, create_metadata_account_v3
        assert_eq!(planned.instructions.len(), 4);
        let md_ix = planned.instructions.last().expect("metadata ix must exist");
        assert_eq!(md_ix.program_id, METADATA_PROGRAM_ID);

        let mut expected = Vec::new();
        expected.push(MPL_CREATE_METADATA_ACCOUNT_V3_DISCRIM);
        expected.extend_from_slice(&borsh_string("Example"));
        expected.extend_from_slice(&borsh_string("EX"));
        expected.extend_from_slice(&borsh_string("https://example.com/token.json"));
        expected.extend_from_slice(&0u16.to_le_bytes());
        expected.push(0u8); // creators: None
        expected.push(0u8); // collection: None
        expected.push(0u8); // uses: None
        expected.push(1u8); // is_mutable
        expected.push(0u8); // collection_details: None

        assert_eq!(md_ix.data, expected);
        assert_eq!(md_ix.accounts.len(), 7);
        assert_eq!(
            md_ix.accounts[0].pubkey,
            derive_metaplex_metadata_pda(&mint)
        );
        assert!(md_ix.accounts[0].is_writable);
        assert_eq!(md_ix.accounts[1].pubkey, mint);
        assert_eq!(md_ix.accounts[2].pubkey, payer);
        assert!(md_ix.accounts[2].is_signer);
        assert_eq!(md_ix.accounts[3].pubkey, payer);
        assert!(md_ix.accounts[3].is_signer);
        assert_eq!(md_ix.accounts[4].pubkey, payer);
        assert!(md_ix.accounts[4].is_signer);
    }

    #[test]
    fn test_plan_spl_token_create_revoke_freeze_requires_freeze_authority() {
        let payer = Pubkey::new_from_array([5u8; 32]);
        let mint = Pubkey::new_from_array([6u8; 32]);

        let err = plan_spl_token_create(
            SplTokenCreatePlan {
                payer,
                mint,
                name: "Example".to_string(),
                symbol: "EX".to_string(),
                uri: "https://example.com/token.json".to_string(),
                decimals: 6,
                initial_supply: 0,
                freeze_authority: false,
                revoke_mint_authority: false,
                revoke_freeze_authority: true,
                metadata_is_mutable: true,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
            123,
        )
        .expect_err("revoke_freeze_authority without freeze_authority must fail");
        assert!(
            err.to_string()
                .contains("revoke_freeze_authority=true requires freeze_authority=true")
        );
    }

    #[test]
    fn test_plan_spl_token_2022_create_derives_payer_ata_and_metadata_is_mint() {
        let payer = Pubkey::new_from_array([7u8; 32]);
        let mint = Pubkey::new_from_array([8u8; 32]);

        let space = spl_token_2022_inline_metadata_required_space(
            payer,
            mint,
            "Name",
            "SYM",
            "https://example.com/meta.json",
        )
        .expect("space calc must succeed");
        assert!(space > SplMint2022::LEN);
        assert_ne!(space, SplMultisig::LEN);

        let planned = plan_spl_token_2022_create(
            SplToken2022CreatePlan {
                payer,
                mint,
                name: "Name".to_string(),
                symbol: "SYM".to_string(),
                uri: "https://example.com/meta.json".to_string(),
                decimals: 9,
                initial_supply: 0,
                freeze_authority: false,
                revoke_mint_authority: false,
                revoke_freeze_authority: false,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
            123,
        )
        .expect("token-2022 create plan must succeed");

        assert_eq!(planned.required_signers, vec![payer, mint]);
        assert_eq!(
            planned.derived.map.get("payer_ata").copied(),
            Some(derive_associated_token_address(
                &payer,
                &mint,
                &TOKEN_2022_PROGRAM_ID
            ))
        );
        assert_eq!(planned.derived.map.get("metadata").copied(), Some(mint));
    }

    #[test]
    fn test_plan_raydium_launchpad_create_token_v2_data_is_deterministic() {
        let program_id = crate::core::cluster::raydium_launchpad_program_id(
            crate::core::cluster::SolanaCluster::MainnetBeta,
        );
        let payer = Pubkey::new_from_array([9u8; 32]);
        let global_config = Pubkey::new_from_array([10u8; 32]);
        let platform_config = Pubkey::new_from_array([11u8; 32]);
        let base_mint = Pubkey::new_from_array([12u8; 32]);
        let quote_mint = Pubkey::new_from_array([13u8; 32]);

        let curve = RaydiumLaunchpadCurveParams::Fixed {
            supply: 1_000_000_000,
            total_quote_fund_raising: 500_000_000,
            migrate_type: 1,
        };
        let vesting = RaydiumLaunchpadVestingParams {
            total_locked_amount: 123,
            cliff_period: 456,
            unlock_period: 789,
        };

        let planned = plan_raydium_launchpad_create(
            RaydiumLaunchpadCreatePlan {
                launchpad_program_id: program_id,
                payer,
                creator: payer,
                global_config,
                platform_config,
                base_mint,
                quote_mint,
                name: "Ray".to_string(),
                symbol: "RAY".to_string(),
                uri: "https://example.com/ray.json".to_string(),
                decimals: 6,
                curve: curve.clone(),
                vesting: vesting.clone(),
                amm_fee_on: RaydiumLaunchpadAmmFeeOn::QuoteToken,
                base_token_program: RaydiumLaunchpadBaseTokenProgram::Token,
                transfer_fee_extension: None,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("raydium launchpad create plan must succeed");

        assert_eq!(planned.required_signers, vec![payer, base_mint]);
        assert!(planned.derived.map.contains_key("metadata"));
        assert!(!planned.derived.map.contains_key("transfer_fee_extension"));

        assert_eq!(planned.instructions.len(), 1);
        let ix = &planned.instructions[0];
        assert_eq!(ix.program_id, program_id);

        let mut expected = Vec::new();
        expected.extend_from_slice(&RAYDIUM_LAUNCHPAD_INITIALIZE_V2_IX_DISCRIM);
        expected.push(6u8);
        expected.extend_from_slice(&borsh_string("Ray"));
        expected.extend_from_slice(&borsh_string("RAY"));
        expected.extend_from_slice(&borsh_string("https://example.com/ray.json"));
        curve.encode_borsh(&mut expected);
        vesting.encode_borsh(&mut expected);
        RaydiumLaunchpadAmmFeeOn::QuoteToken.encode_borsh(&mut expected);

        assert_eq!(ix.data, expected);
    }

    #[test]
    fn test_plan_raydium_launchpad_create_token_2022_data_includes_transfer_fee_option() {
        let program_id = crate::core::cluster::raydium_launchpad_program_id(
            crate::core::cluster::SolanaCluster::MainnetBeta,
        );
        let payer = Pubkey::new_from_array([14u8; 32]);
        let global_config = Pubkey::new_from_array([15u8; 32]);
        let platform_config = Pubkey::new_from_array([16u8; 32]);
        let base_mint = Pubkey::new_from_array([17u8; 32]);
        let quote_mint = Pubkey::new_from_array([18u8; 32]);

        let curve = RaydiumLaunchpadCurveParams::Linear {
            supply: 1_000_000_000,
            total_quote_fund_raising: 42,
            migrate_type: 1,
        };
        let vesting = RaydiumLaunchpadVestingParams {
            total_locked_amount: 1,
            cliff_period: 2,
            unlock_period: 3,
        };
        let fee = RaydiumLaunchpadTransferFeeExtensionParams {
            transfer_fee_basis_points: 250,
            maximum_fee: 1_000_000,
        };

        let planned = plan_raydium_launchpad_create(
            RaydiumLaunchpadCreatePlan {
                launchpad_program_id: program_id,
                payer,
                creator: payer,
                global_config,
                platform_config,
                base_mint,
                quote_mint,
                name: "Ray2022".to_string(),
                symbol: "R22".to_string(),
                uri: "https://example.com/r22.json".to_string(),
                decimals: 9,
                curve: curve.clone(),
                vesting: vesting.clone(),
                amm_fee_on: RaydiumLaunchpadAmmFeeOn::BothToken,
                base_token_program: RaydiumLaunchpadBaseTokenProgram::Token2022,
                transfer_fee_extension: Some(fee.clone()),
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("raydium launchpad token-2022 create plan must succeed");

        assert_eq!(planned.required_signers, vec![payer, base_mint]);
        assert!(!planned.derived.map.contains_key("metadata"));

        let ix = &planned.instructions[0];
        assert_eq!(ix.program_id, program_id);
        let mut expected = Vec::new();
        expected.extend_from_slice(&RAYDIUM_LAUNCHPAD_INITIALIZE_WITH_TOKEN_2022_IX_DISCRIM);
        expected.push(9u8);
        expected.extend_from_slice(&borsh_string("Ray2022"));
        expected.extend_from_slice(&borsh_string("R22"));
        expected.extend_from_slice(&borsh_string("https://example.com/r22.json"));
        curve.encode_borsh(&mut expected);
        vesting.encode_borsh(&mut expected);
        RaydiumLaunchpadAmmFeeOn::BothToken.encode_borsh(&mut expected);
        expected.push(1u8);
        fee.encode_borsh(&mut expected);

        assert_eq!(ix.data, expected);
    }
}
