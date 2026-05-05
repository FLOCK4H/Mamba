use {
    crate::{
        core::{
            create::{
                ComputeBudgetPlan, DerivedAddresses, RENT_SYSVAR_ID,
                derive_associated_token_address, derive_metaplex_metadata_pda,
            },
            sol::{
                ATA_PROGRAM_ID, METADATA_PROGRAM_ID, SYSTEM_PROGRAM, TOKEN_2022_PROGRAM_ID,
                TOKEN_PROGRAM_ID, WSOL_MINT,
            },
        },
        dex::{
            meteora_dbc::{
                EVENT_AUTHORITY_SEED as METEORA_DBC_EVENT_AUTHORITY_SEED, METEORA_DBC_ID,
                METEORA_DBC_POOL_AUTHORITY, POOL_AUTHORITY_SEED as METEORA_DBC_POOL_AUTHORITY_SEED,
                POOL_PREFIX as METEORA_DBC_POOL_PREFIX,
                TOKEN_VAULT_PREFIX as METEORA_DBC_TOKEN_VAULT_PREFIX,
            },
            meteora_dlmm::METEORA_DLMM_ID,
            pump_swap::{GLOBAL_CONFIG_PUB, PUMP_SWAP_ID},
            raydium_clmm::RAYDIUM_CLMM_CREATE_POOL_IX_DISCRIM,
            raydium_cpmm::{
                OBSERVATION_SEED, POOL_LP_MINT_SEED, POOL_SEED, POOL_VAULT_SEED,
                RAYDIUM_CPMM_INITIALIZE_IX_DISCRIM,
            },
        },
    },
    anchor_lang::{InstructionData, ToAccountMetas},
    anyhow::{Context, bail},
    meteora_dlmm_types as dlmm_idl, pump_swap_types as pump_swap_idl,
    solana_program::{
        instruction::{AccountMeta, Instruction},
        pubkey::Pubkey,
    },
    solana_system_interface::instruction as system_instruction_if,
    spl_associated_token_account::instruction::create_associated_token_account_idempotent,
    spl_token_2022::instruction::sync_native,
};

#[derive(Debug, Clone)]
pub struct PlannedPoolTx {
    pub payer: Pubkey,
    pub required_signers: Vec<Pubkey>,
    pub derived: DerivedAddresses,
    pub priority_fee_addresses: Vec<Pubkey>,
    pub instructions: Vec<Instruction>,
}

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

#[derive(Debug, Clone)]
pub struct PumpSwapCreatePoolPlan {
    pub payer: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_token_program: Pubkey,
    pub quote_token_program: Pubkey,
    pub index: u16,
    pub base_amount_in: u64,
    pub quote_amount_in: u64,
    pub coin_creator: Pubkey,
    pub is_mayhem_mode: bool,
}

pub fn plan_pump_swap_create_pool(
    plan: PumpSwapCreatePoolPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedPoolTx> {
    if plan.base_mint == Pubkey::default() || plan.quote_mint == Pubkey::default() {
        bail!("base_mint and quote_mint must be valid pubkeys");
    }
    if plan.base_mint == plan.quote_mint {
        bail!("base_mint must differ from quote_mint");
    }
    if plan.base_amount_in == 0 || plan.quote_amount_in == 0 {
        bail!("base_amount_in and quote_amount_in must be > 0");
    }

    let (pool, _bump) = Pubkey::find_program_address(
        &[
            b"pool",
            &plan.index.to_le_bytes(),
            plan.payer.as_ref(),
            plan.base_mint.as_ref(),
            plan.quote_mint.as_ref(),
        ],
        &PUMP_SWAP_ID,
    );
    let lp_mint = Pubkey::find_program_address(&[b"pool_lp_mint", pool.as_ref()], &PUMP_SWAP_ID).0;
    let event_authority = Pubkey::find_program_address(&[b"__event_authority"], &PUMP_SWAP_ID).0;

    let user_base_token_account =
        derive_associated_token_address(&plan.payer, &plan.base_mint, &plan.base_token_program);
    let user_quote_token_account =
        derive_associated_token_address(&plan.payer, &plan.quote_mint, &plan.quote_token_program);
    let user_pool_token_account =
        derive_associated_token_address(&plan.payer, &lp_mint, &TOKEN_2022_PROGRAM_ID);

    let pool_base_token_account =
        derive_associated_token_address(&pool, &plan.base_mint, &plan.base_token_program);
    let pool_quote_token_account =
        derive_associated_token_address(&pool, &plan.quote_mint, &plan.quote_token_program);

    let mut instructions = Vec::new();

    // Ensure user ATAs exist.
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.base_mint,
        &plan.base_token_program,
    ));
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.quote_mint,
        &plan.quote_token_program,
    ));

    // Wrap WSOL (if either side is WSOL).
    if plan.base_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &user_base_token_account,
            plan.base_amount_in,
        ));
        instructions.push(sync_native(
            &plan.base_token_program,
            &user_base_token_account,
        )?);
    }
    if plan.quote_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &user_quote_token_account,
            plan.quote_amount_in,
        ));
        instructions.push(sync_native(
            &plan.quote_token_program,
            &user_quote_token_account,
        )?);
    }

    let accounts = pump_swap_idl::accounts::CreatePool {
        pool: to_anchor_pubkey(pool),
        global_config: to_anchor_pubkey(GLOBAL_CONFIG_PUB),
        creator: to_anchor_pubkey(plan.payer),
        base_mint: to_anchor_pubkey(plan.base_mint),
        quote_mint: to_anchor_pubkey(plan.quote_mint),
        lp_mint: to_anchor_pubkey(lp_mint),
        user_base_token_account: to_anchor_pubkey(user_base_token_account),
        user_quote_token_account: to_anchor_pubkey(user_quote_token_account),
        user_pool_token_account: to_anchor_pubkey(user_pool_token_account),
        pool_base_token_account: to_anchor_pubkey(pool_base_token_account),
        pool_quote_token_account: to_anchor_pubkey(pool_quote_token_account),
        system_program: to_anchor_pubkey(SYSTEM_PROGRAM),
        token_2022_program: to_anchor_pubkey(TOKEN_2022_PROGRAM_ID),
        base_token_program: to_anchor_pubkey(plan.base_token_program),
        quote_token_program: to_anchor_pubkey(plan.quote_token_program),
        associated_token_program: to_anchor_pubkey(ATA_PROGRAM_ID),
        event_authority: to_anchor_pubkey(event_authority),
        program: to_anchor_pubkey(PUMP_SWAP_ID),
    };
    let ix_data = pump_swap_idl::instruction::CreatePool {
        _index: plan.index,
        _base_amount_in: plan.base_amount_in,
        _quote_amount_in: plan.quote_amount_in,
        _coin_creator: to_anchor_pubkey(plan.coin_creator),
        _is_mayhem_mode: plan.is_mayhem_mode,
    };
    instructions.push(Instruction {
        program_id: PUMP_SWAP_ID,
        accounts: to_solana_account_metas(accounts.to_account_metas(None)),
        data: ix_data.data(),
    });

    compute_budget.prepend_instructions(&mut instructions);

    Ok::<PlannedPoolTx, anyhow::Error>(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer],
        derived: DerivedAddresses::new()
            .insert("pool", pool)
            .insert("lp_mint", lp_mint)
            .insert("user_base_token_account", user_base_token_account)
            .insert("user_quote_token_account", user_quote_token_account)
            .insert("user_pool_token_account", user_pool_token_account)
            .insert("pool_base_token_account", pool_base_token_account)
            .insert("pool_quote_token_account", pool_quote_token_account)
            .insert("event_authority", event_authority),
        priority_fee_addresses: vec![
            PUMP_SWAP_ID,
            plan.base_token_program,
            plan.quote_token_program,
            TOKEN_2022_PROGRAM_ID,
            ATA_PROGRAM_ID,
        ],
        instructions,
    })
}

#[derive(Debug, Clone)]
pub struct RaydiumCpmmCreatePoolPlan {
    pub payer: Pubkey,
    pub program_id: Pubkey,
    pub amm_config: Pubkey,
    pub create_pool_fee: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub token_0_program: Pubkey,
    pub token_1_program: Pubkey,
    pub init_amount_0: u64,
    pub init_amount_1: u64,
    pub open_time: u64,
}

pub fn plan_raydium_cpmm_create_pool(
    plan: RaydiumCpmmCreatePoolPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedPoolTx> {
    anyhow::ensure!(
        plan.token_0_mint < plan.token_1_mint,
        "raydium cpmm requires token_0_mint < token_1_mint"
    );
    anyhow::ensure!(
        plan.init_amount_0 > 0 && plan.init_amount_1 > 0,
        "init amounts must be > 0"
    );

    let authority =
        Pubkey::find_program_address(&[crate::dex::raydium_cpmm::AUTH_SEED], &plan.program_id).0;
    let pool_state = Pubkey::find_program_address(
        &[
            POOL_SEED,
            plan.amm_config.as_ref(),
            plan.token_0_mint.as_ref(),
            plan.token_1_mint.as_ref(),
        ],
        &plan.program_id,
    )
    .0;
    let lp_mint =
        Pubkey::find_program_address(&[POOL_LP_MINT_SEED, pool_state.as_ref()], &plan.program_id).0;
    let creator_token_0 =
        derive_associated_token_address(&plan.payer, &plan.token_0_mint, &plan.token_0_program);
    let creator_token_1 =
        derive_associated_token_address(&plan.payer, &plan.token_1_mint, &plan.token_1_program);
    let creator_lp_token =
        derive_associated_token_address(&plan.payer, &lp_mint, &TOKEN_PROGRAM_ID);
    let token_0_vault = Pubkey::find_program_address(
        &[
            POOL_VAULT_SEED,
            pool_state.as_ref(),
            plan.token_0_mint.as_ref(),
        ],
        &plan.program_id,
    )
    .0;
    let token_1_vault = Pubkey::find_program_address(
        &[
            POOL_VAULT_SEED,
            pool_state.as_ref(),
            plan.token_1_mint.as_ref(),
        ],
        &plan.program_id,
    )
    .0;
    let observation_state =
        Pubkey::find_program_address(&[OBSERVATION_SEED, pool_state.as_ref()], &plan.program_id).0;

    let mut instructions = Vec::new();

    // Ensure user ATAs exist.
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_0_mint,
        &plan.token_0_program,
    ));
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_1_mint,
        &plan.token_1_program,
    ));

    // Wrap WSOL (if either side is WSOL).
    if plan.token_0_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &creator_token_0,
            plan.init_amount_0,
        ));
        instructions.push(sync_native(&TOKEN_PROGRAM_ID, &creator_token_0)?);
    }
    if plan.token_1_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &creator_token_1,
            plan.init_amount_1,
        ));
        instructions.push(sync_native(&TOKEN_PROGRAM_ID, &creator_token_1)?);
    }

    let mut data = Vec::with_capacity(8 + 8 + 8 + 8);
    data.extend_from_slice(&RAYDIUM_CPMM_INITIALIZE_IX_DISCRIM);
    data.extend_from_slice(&plan.init_amount_0.to_le_bytes());
    data.extend_from_slice(&plan.init_amount_1.to_le_bytes());
    data.extend_from_slice(&plan.open_time.to_le_bytes());

    instructions.push(Instruction {
        program_id: plan.program_id,
        accounts: vec![
            AccountMeta::new(plan.payer, true),
            AccountMeta::new_readonly(plan.amm_config, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(pool_state, false),
            AccountMeta::new_readonly(plan.token_0_mint, false),
            AccountMeta::new_readonly(plan.token_1_mint, false),
            AccountMeta::new(lp_mint, false),
            AccountMeta::new(creator_token_0, false),
            AccountMeta::new(creator_token_1, false),
            AccountMeta::new(creator_lp_token, false),
            AccountMeta::new(token_0_vault, false),
            AccountMeta::new(token_1_vault, false),
            AccountMeta::new(plan.create_pool_fee, false),
            AccountMeta::new(observation_state, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(plan.token_0_program, false),
            AccountMeta::new_readonly(plan.token_1_program, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
        ],
        data,
    });

    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer],
        derived: DerivedAddresses::new()
            .insert("authority", authority)
            .insert("pool_state", pool_state)
            .insert("lp_mint", lp_mint)
            .insert("creator_token_0", creator_token_0)
            .insert("creator_token_1", creator_token_1)
            .insert("creator_lp_token", creator_lp_token)
            .insert("token_0_vault", token_0_vault)
            .insert("token_1_vault", token_1_vault)
            .insert("observation_state", observation_state),
        priority_fee_addresses: vec![
            plan.program_id,
            TOKEN_PROGRAM_ID,
            plan.token_0_program,
            plan.token_1_program,
            ATA_PROGRAM_ID,
        ],
        instructions,
    })
}

#[derive(Debug, Clone)]
pub struct RaydiumClmmCreatePoolPlan {
    pub payer: Pubkey,
    pub program_id: Pubkey,
    pub amm_config: Pubkey,
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,
    pub token_program_0: Pubkey,
    pub token_program_1: Pubkey,
    pub sqrt_price_x64: u128,
    pub open_time: u64,
}

pub fn plan_raydium_clmm_create_pool(
    plan: RaydiumClmmCreatePoolPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedPoolTx> {
    anyhow::ensure!(
        plan.token_mint_0 < plan.token_mint_1,
        "raydium clmm requires token_mint_0 < token_mint_1"
    );
    anyhow::ensure!(plan.sqrt_price_x64 > 0, "sqrt_price_x64 must be > 0");

    let pool_state = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::POOL_SEED,
            plan.amm_config.as_ref(),
            plan.token_mint_0.as_ref(),
            plan.token_mint_1.as_ref(),
        ],
        &plan.program_id,
    )
    .0;
    let token_vault_0 = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::POOL_VAULT_SEED,
            pool_state.as_ref(),
            plan.token_mint_0.as_ref(),
        ],
        &plan.program_id,
    )
    .0;
    let token_vault_1 = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::POOL_VAULT_SEED,
            pool_state.as_ref(),
            plan.token_mint_1.as_ref(),
        ],
        &plan.program_id,
    )
    .0;
    let observation_state = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::OBSERVATION_SEED,
            pool_state.as_ref(),
        ],
        &plan.program_id,
    )
    .0;
    let tick_array_bitmap = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::POOL_TICK_ARRAY_BITMAP_SEED,
            pool_state.as_ref(),
        ],
        &plan.program_id,
    )
    .0;

    let mut data = Vec::with_capacity(8 + 16 + 8);
    data.extend_from_slice(&RAYDIUM_CLMM_CREATE_POOL_IX_DISCRIM);
    data.extend_from_slice(&plan.sqrt_price_x64.to_le_bytes());
    data.extend_from_slice(&plan.open_time.to_le_bytes());

    let mut instructions = vec![Instruction {
        program_id: plan.program_id,
        accounts: vec![
            AccountMeta::new(plan.payer, true),
            AccountMeta::new_readonly(plan.amm_config, false),
            AccountMeta::new(pool_state, false),
            AccountMeta::new_readonly(plan.token_mint_0, false),
            AccountMeta::new_readonly(plan.token_mint_1, false),
            AccountMeta::new(token_vault_0, false),
            AccountMeta::new(token_vault_1, false),
            AccountMeta::new(observation_state, false),
            AccountMeta::new(tick_array_bitmap, false),
            AccountMeta::new_readonly(plan.token_program_0, false),
            AccountMeta::new_readonly(plan.token_program_1, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
        ],
        data,
    }];
    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer],
        derived: DerivedAddresses::new()
            .insert("pool_state", pool_state)
            .insert("token_vault_0", token_vault_0)
            .insert("token_vault_1", token_vault_1)
            .insert("observation_state", observation_state)
            .insert("tick_array_bitmap", tick_array_bitmap),
        priority_fee_addresses: vec![
            plan.program_id,
            plan.token_program_0,
            plan.token_program_1,
            SYSTEM_PROGRAM,
        ],
        instructions,
    })
}

#[derive(Debug, Clone)]
pub struct RaydiumClmmSeedLiquidityPlan {
    pub payer: Pubkey,
    pub program_id: Pubkey,
    pub pool_state: Pubkey,
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,
    pub token_program_0: Pubkey,
    pub token_program_1: Pubkey,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
    pub position_nft_mint: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_amount_in: u64,
    pub quote_amount_in: u64,
    pub sqrt_price_x64: u128,
    pub tick_spacing: u16,
    pub with_metadata: bool,
    /// When set, forces the Raydium CLMM `open_position` "base side" selection. Use this when
    /// callers want the user-provided side to be treated as exact (and the other side as max).
    ///
    /// When unset, the builder auto-selects the limiting side (given max amounts and price) to
    /// avoid `PriceSlippageCheck` failures when the user maxima are imbalanced.
    pub open_position_base_is_mint0_override: Option<bool>,
}

pub fn plan_raydium_clmm_seed_liquidity(
    plan: RaydiumClmmSeedLiquidityPlan,
) -> anyhow::Result<PlannedPoolTx> {
    anyhow::ensure!(
        plan.token_program_0 == TOKEN_PROGRAM_ID && plan.token_program_1 == TOKEN_PROGRAM_ID,
        "raydium clmm seed liquidity currently supports legacy spl_token mints only"
    );
    anyhow::ensure!(plan.base_amount_in > 0, "base_amount must be > 0");
    anyhow::ensure!(plan.quote_amount_in > 0, "quote_amount must be > 0");
    anyhow::ensure!(plan.tick_spacing > 0, "tick_spacing must be > 0");
    anyhow::ensure!(
        plan.base_mint == plan.token_mint_0 || plan.base_mint == plan.token_mint_1,
        "raydium clmm seed base_mint must be one of the pool mints"
    );
    anyhow::ensure!(
        plan.quote_mint == plan.token_mint_0 || plan.quote_mint == plan.token_mint_1,
        "raydium clmm seed quote_mint must be one of the pool mints"
    );
    anyhow::ensure!(
        plan.base_mint != plan.quote_mint,
        "raydium clmm seed base_mint must differ from quote_mint"
    );

    let spacing = plan.tick_spacing as i32;

    fn div_floor_i32(a: i32, b: i32) -> i32 {
        let q = a / b;
        let r = a % b;
        if r < 0 { q - 1 } else { q }
    }

    fn floor_to_multiple(value: i32, step: i32) -> i32 {
        div_floor_i32(value, step) * step
    }

    fn div_ceil_i32(a: i32, b: i32) -> i32 {
        let q = a / b;
        let r = a % b;
        if r > 0 { q + 1 } else { q }
    }

    fn ceil_to_multiple(value: i32, step: i32) -> i32 {
        div_ceil_i32(value, step) * step
    }

    fn tick_array_start_index_by_tick(tick: i32, tick_spacing: i32) -> i32 {
        let ticks_per_array = tick_spacing * crate::dex::raydium_clmm::TICK_ARRAY_SIZE;
        div_floor_i32(tick, ticks_per_array) * ticks_per_array
    }

    let sqrt_price = (plan.sqrt_price_x64 as f64) / 2_f64.powi(64);
    let price = sqrt_price * sqrt_price;
    anyhow::ensure!(
        price.is_finite() && price > 0.0,
        "sqrt_price_x64 produced invalid price"
    );

    let tick_estimate = (price.ln() / 1.0001_f64.ln()).floor() as i32;
    let tick_center = floor_to_multiple(tick_estimate, spacing);

    const DEFAULT_PRICE_RANGE_MULTIPLIER: f64 = 2.0; // ~0.5x..2x around initial price
    let tick_range = (DEFAULT_PRICE_RANGE_MULTIPLIER.ln() / 1.0001_f64.ln()).ceil() as i32;

    let mut tick_lower = floor_to_multiple(tick_center.saturating_sub(tick_range), spacing);
    let mut tick_upper = ceil_to_multiple(tick_center.saturating_add(tick_range), spacing);
    if tick_upper <= tick_lower {
        tick_upper = tick_lower.saturating_add(spacing);
    }
    tick_lower = tick_lower.max(crate::dex::raydium_clmm::CLMM_MIN_TICK);
    tick_upper = tick_upper.min(crate::dex::raydium_clmm::CLMM_MAX_TICK);
    anyhow::ensure!(tick_upper > tick_lower, "invalid raydium clmm tick range");

    let tick_array_lower_start_index = tick_array_start_index_by_tick(tick_lower, spacing);
    let tick_array_upper_start_index = tick_array_start_index_by_tick(tick_upper, spacing);

    let tick_array_lower = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::TICK_ARRAY_SEED,
            plan.pool_state.as_ref(),
            &tick_array_lower_start_index.to_be_bytes(),
        ],
        &plan.program_id,
    )
    .0;
    let tick_array_upper = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::TICK_ARRAY_SEED,
            plan.pool_state.as_ref(),
            &tick_array_upper_start_index.to_be_bytes(),
        ],
        &plan.program_id,
    )
    .0;

    let position_nft_account =
        derive_associated_token_address(&plan.payer, &plan.position_nft_mint, &TOKEN_PROGRAM_ID);
    let metadata_account = derive_metaplex_metadata_pda(&plan.position_nft_mint);
    let personal_position = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::POSITION_SEED,
            plan.position_nft_mint.as_ref(),
        ],
        &plan.program_id,
    )
    .0;
    let protocol_position = Pubkey::find_program_address(
        &[
            crate::dex::raydium_clmm::POSITION_SEED,
            plan.pool_state.as_ref(),
            &tick_lower.to_le_bytes(),
            &tick_upper.to_le_bytes(),
        ],
        &plan.program_id,
    )
    .0;

    let owner_token_account_0 =
        derive_associated_token_address(&plan.payer, &plan.token_mint_0, &plan.token_program_0);
    let owner_token_account_1 =
        derive_associated_token_address(&plan.payer, &plan.token_mint_1, &plan.token_program_1);

    // The Raydium CLMM `open_position` instruction uses a "base token" flag to decide which side
    // to treat as the primary amount when computing liquidity.
    //
    // For robustness, auto-select the limiting side (given current price + provided maxima) so
    // the computed other-side amount stays <= the provided max and avoids `PriceSlippageCheck`.
    //
    // For UX, callers may override this behavior when one side is user-specified and the other
    // side was computed, so the on-chain deposit matches what the user typed (within rounding).
    let base_mint_is_mint0 = plan.base_mint == plan.token_mint_0;
    let (amount_max_0, amount_max_1) = if base_mint_is_mint0 {
        (plan.base_amount_in, plan.quote_amount_in)
    } else {
        (plan.quote_amount_in, plan.base_amount_in)
    };
    let base_is_mint0 = if let Some(forced) = plan.open_position_base_is_mint0_override {
        forced
    } else {
        let ratio_user = (amount_max_1 as f64) / (amount_max_0 as f64);
        ratio_user >= price
    };

    let mut instructions = Vec::new();

    // Ensure payer ATAs exist.
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_mint_0,
        &plan.token_program_0,
    ));
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_mint_1,
        &plan.token_program_1,
    ));

    // Wrap WSOL if needed.
    if plan.token_mint_0 == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &owner_token_account_0,
            amount_max_0,
        ));
        instructions.push(sync_native(&plan.token_program_0, &owner_token_account_0)?);
    }
    if plan.token_mint_1 == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &owner_token_account_1,
            amount_max_1,
        ));
        instructions.push(sync_native(&plan.token_program_1, &owner_token_account_1)?);
    }

    // open_position (from base amounts)
    let mut data = Vec::with_capacity(8 + 64);
    data.extend_from_slice(&crate::dex::raydium_clmm::RAYDIUM_CLMM_OPEN_POSITION_IX_DISCRIM);
    data.extend_from_slice(&tick_lower.to_le_bytes());
    data.extend_from_slice(&tick_upper.to_le_bytes());
    data.extend_from_slice(&tick_array_lower_start_index.to_le_bytes());
    data.extend_from_slice(&tick_array_upper_start_index.to_le_bytes());
    data.extend_from_slice(&0u128.to_le_bytes()); // liquidity (computed by program)
    data.extend_from_slice(&amount_max_0.to_le_bytes());
    data.extend_from_slice(&amount_max_1.to_le_bytes());
    data.push(if plan.with_metadata { 1 } else { 0 });
    data.push(1u8); // optionBaseFlag
    data.push(if base_is_mint0 { 1 } else { 0 });

    instructions.push(Instruction {
        program_id: plan.program_id,
        accounts: vec![
            AccountMeta::new(plan.payer, true),
            AccountMeta::new_readonly(plan.payer, false),
            AccountMeta::new(plan.position_nft_mint, true),
            AccountMeta::new(position_nft_account, false),
            AccountMeta::new(metadata_account, false),
            AccountMeta::new(plan.pool_state, false),
            AccountMeta::new(protocol_position, false),
            AccountMeta::new(tick_array_lower, false),
            AccountMeta::new(tick_array_upper, false),
            AccountMeta::new(personal_position, false),
            AccountMeta::new(owner_token_account_0, false),
            AccountMeta::new(owner_token_account_1, false),
            AccountMeta::new(plan.token_vault_0, false),
            AccountMeta::new(plan.token_vault_1, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(METADATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(plan.token_mint_0, false),
            AccountMeta::new_readonly(plan.token_mint_1, false),
        ],
        data,
    });

    Ok(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer, plan.position_nft_mint],
        derived: DerivedAddresses::new()
            .insert("position_nft_mint", plan.position_nft_mint)
            .insert("position_nft_account", position_nft_account)
            .insert("position_nft_metadata", metadata_account)
            .insert("personal_position", personal_position)
            .insert("protocol_position", protocol_position)
            .insert("tick_array_lower", tick_array_lower)
            .insert("tick_array_upper", tick_array_upper),
        priority_fee_addresses: vec![
            plan.program_id,
            TOKEN_PROGRAM_ID,
            ATA_PROGRAM_ID,
            METADATA_PROGRAM_ID,
            SYSTEM_PROGRAM,
            plan.pool_state,
            plan.position_nft_mint,
        ],
        instructions,
    })
}

#[derive(Debug, Clone)]
pub struct MeteoraDlmmCreatePoolPlan {
    pub payer: Pubkey,
    pub lb_pair: Pubkey,
    pub token_mint_x: Pubkey,
    pub token_mint_y: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub oracle: Pubkey,
    pub user_token_x: Pubkey,
    pub user_token_y: Pubkey,
    pub token_program_x: Pubkey,
    pub token_program_y: Pubkey,
    pub token_badge_x: Pubkey,
    pub token_badge_y: Pubkey,
    pub bin_array_bitmap_extension: Pubkey,
    pub params: dlmm_idl::CustomizableParams,
    /// When either side is WSOL, optionally wrap lamports into the corresponding user ATA
    /// before initializing the pool. Use this to satisfy the launch proof check and/or to
    /// pre-fund the ATA for immediate liquidity seeding.
    pub wsol_wrap_lamports_x: u64,
    pub wsol_wrap_lamports_y: u64,
}

pub fn plan_meteora_dlmm_create_pool(
    plan: MeteoraDlmmCreatePoolPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedPoolTx> {
    if plan.lb_pair == Pubkey::default() {
        bail!("lb_pair must be a valid pubkey");
    }
    let event_authority = Pubkey::find_program_address(
        &[crate::dex::meteora_dlmm::EVENT_AUTHORITY_SEED],
        &METEORA_DLMM_ID,
    )
    .0;

    let mut instructions = Vec::new();
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_mint_x,
        &plan.token_program_x,
    ));
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_mint_y,
        &plan.token_program_y,
    ));

    // Meteora DLMM permissionless launch pools enforce a "token launch owner proof" check.
    // Empirically, this can require non-zero balances in the user's token accounts; when WSOL is
    // used as one side, the associated token account is often empty. Wrap a tiny amount (or a
    // caller-provided amount) to satisfy the proof with minimal cost.
    if plan.token_mint_x == WSOL_MINT && plan.wsol_wrap_lamports_x > 0 {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &plan.user_token_x,
            plan.wsol_wrap_lamports_x,
        ));
        instructions.push(sync_native(&plan.token_program_x, &plan.user_token_x)?);
    }
    if plan.token_mint_y == WSOL_MINT && plan.wsol_wrap_lamports_y > 0 {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &plan.user_token_y,
            plan.wsol_wrap_lamports_y,
        ));
        instructions.push(sync_native(&plan.token_program_y, &plan.user_token_y)?);
    }

    let accounts = dlmm_idl::accounts::InitializeCustomizablePermissionlessLbPair2 {
        lb_pair: to_anchor_pubkey(plan.lb_pair),
        bin_array_bitmap_extension: to_anchor_pubkey(plan.bin_array_bitmap_extension),
        token_mint_x: to_anchor_pubkey(plan.token_mint_x),
        token_mint_y: to_anchor_pubkey(plan.token_mint_y),
        reserve_x: to_anchor_pubkey(plan.reserve_x),
        reserve_y: to_anchor_pubkey(plan.reserve_y),
        oracle: to_anchor_pubkey(plan.oracle),
        user_token_x: to_anchor_pubkey(plan.user_token_x),
        funder: to_anchor_pubkey(plan.payer),
        token_badge_x: to_anchor_pubkey(plan.token_badge_x),
        token_badge_y: to_anchor_pubkey(plan.token_badge_y),
        token_program_x: to_anchor_pubkey(plan.token_program_x),
        token_program_y: to_anchor_pubkey(plan.token_program_y),
        system_program: to_anchor_pubkey(SYSTEM_PROGRAM),
        user_token_y: to_anchor_pubkey(plan.user_token_y),
        event_authority: to_anchor_pubkey(event_authority),
        program: to_anchor_pubkey(METEORA_DLMM_ID),
    };
    let ix_data = dlmm_idl::instruction::InitializeCustomizablePermissionlessLbPair2 {
        _params: plan.params,
    };
    instructions.push(Instruction {
        program_id: METEORA_DLMM_ID,
        accounts: to_solana_account_metas(accounts.to_account_metas(None)),
        data: ix_data.data(),
    });

    compute_budget.prepend_instructions(&mut instructions);

    Ok(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer],
        derived: DerivedAddresses::new()
            .insert("lb_pair", plan.lb_pair)
            .insert("reserve_x", plan.reserve_x)
            .insert("reserve_y", plan.reserve_y)
            .insert("oracle", plan.oracle)
            .insert("event_authority", event_authority),
        priority_fee_addresses: vec![
            METEORA_DLMM_ID,
            plan.token_program_x,
            plan.token_program_y,
            SYSTEM_PROGRAM,
        ],
        instructions,
    })
}

#[derive(Debug, Clone)]
pub struct MeteoraDlmmSeedLiquidityPlan {
    pub payer: Pubkey,
    pub lb_pair: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub user_token_x: Pubkey,
    pub user_token_y: Pubkey,
    pub token_mint_x: Pubkey,
    pub token_mint_y: Pubkey,
    pub token_program_x: Pubkey,
    pub token_program_y: Pubkey,
    pub bin_array_bitmap_extension: Pubkey,
    pub active_id: i32,
    pub amount_x: u64,
    pub amount_y: u64,
    /// Number of bins the position spans. For bootstrapping, `1` (active bin only) is recommended.
    pub width: i32,
}

pub fn plan_meteora_dlmm_seed_liquidity(
    plan: MeteoraDlmmSeedLiquidityPlan,
) -> anyhow::Result<PlannedPoolTx> {
    anyhow::ensure!(plan.amount_x > 0, "meteora dlmm seed amount_x must be > 0");
    anyhow::ensure!(plan.amount_y > 0, "meteora dlmm seed amount_y must be > 0");
    anyhow::ensure!(plan.width > 0, "meteora dlmm seed width must be > 0");

    const MAX_BIN_PER_ARRAY: i32 = 70; // matches `src/dex/meteora_dlmm.rs`
    fn bin_id_to_bin_array_index(bin_id: i32) -> i32 {
        let idx = bin_id / MAX_BIN_PER_ARRAY;
        let rem = bin_id % MAX_BIN_PER_ARRAY;
        if bin_id.is_negative() && rem != 0 {
            idx - 1
        } else {
            idx
        }
    }

    let event_authority = Pubkey::find_program_address(
        &[crate::dex::meteora_dlmm::EVENT_AUTHORITY_SEED],
        &METEORA_DLMM_ID,
    )
    .0;

    // Use the PDA position path to avoid introducing extra required signers beyond payer.
    let lower_bin_id = plan.active_id;
    let width = plan.width;
    let position = Pubkey::find_program_address(
        &[
            b"position",
            plan.lb_pair.as_ref(),
            plan.payer.as_ref(),
            &lower_bin_id.to_le_bytes(),
            &width.to_le_bytes(),
        ],
        &METEORA_DLMM_ID,
    )
    .0;

    let mut derived = DerivedAddresses::new().insert("position", position);

    // Initialize bin arrays spanning the position range.
    let upper_bin_id = lower_bin_id
        .checked_add(width.saturating_sub(1))
        .context("meteora dlmm seed position upper_bin_id overflow")?;
    let start_idx = bin_id_to_bin_array_index(lower_bin_id);
    let end_idx = bin_id_to_bin_array_index(upper_bin_id);
    let (min_idx, max_idx) = if start_idx <= end_idx {
        (start_idx, end_idx)
    } else {
        (end_idx, start_idx)
    };

    let mut bin_arrays: Vec<(i32, Pubkey)> = Vec::new();
    for idx in min_idx..=max_idx {
        let pda = Pubkey::find_program_address(
            &[
                crate::dex::meteora_dlmm::BIN_ARRAY_SEED,
                plan.lb_pair.as_ref(),
                &(idx as i64).to_le_bytes(),
            ],
            &METEORA_DLMM_ID,
        )
        .0;
        derived = derived.insert(format!("bin_array_{idx}"), pda);
        bin_arrays.push((idx, pda));
    }

    let mut instructions = Vec::new();

    // initialize_position_pda
    let position_accounts = dlmm_idl::accounts::InitializePositionPda {
        payer: to_anchor_pubkey(plan.payer),
        base: to_anchor_pubkey(plan.payer),
        position: to_anchor_pubkey(position),
        lb_pair: to_anchor_pubkey(plan.lb_pair),
        owner: to_anchor_pubkey(plan.payer),
        rent: to_anchor_pubkey(RENT_SYSVAR_ID),
        system_program: to_anchor_pubkey(SYSTEM_PROGRAM),
        event_authority: to_anchor_pubkey(event_authority),
        program: to_anchor_pubkey(METEORA_DLMM_ID),
    };
    let position_ix = dlmm_idl::instruction::InitializePositionPda {
        _lower_bin_id: lower_bin_id,
        _width: width,
    };
    instructions.push(Instruction {
        program_id: METEORA_DLMM_ID,
        accounts: to_solana_account_metas(position_accounts.to_account_metas(None)),
        data: position_ix.data(),
    });

    // initialize_bin_array for each required index
    for (idx, bin_array) in bin_arrays.iter().copied() {
        let init_accounts = dlmm_idl::accounts::InitializeBinArray {
            lb_pair: to_anchor_pubkey(plan.lb_pair),
            bin_array: to_anchor_pubkey(bin_array),
            funder: to_anchor_pubkey(plan.payer),
            system_program: to_anchor_pubkey(SYSTEM_PROGRAM),
        };
        let init_ix = dlmm_idl::instruction::InitializeBinArray { _index: idx as i64 };
        instructions.push(Instruction {
            program_id: METEORA_DLMM_ID,
            accounts: to_solana_account_metas(init_accounts.to_account_metas(None)),
            data: init_ix.data(),
        });
    }

    // add_liquidity2 (remaining accounts = bin arrays; RemainingAccountsInfo slices empty)
    let liquidity_parameter = dlmm_idl::LiquidityParameter {
        amount_x: plan.amount_x,
        amount_y: plan.amount_y,
        bin_liquidity_dist: vec![dlmm_idl::BinLiquidityDistribution {
            bin_id: plan.active_id,
            distribution_x: 10_000,
            distribution_y: 10_000,
        }],
    };
    let remaining_accounts_info = dlmm_idl::RemainingAccountsInfo { slices: Vec::new() };
    let add_ix = dlmm_idl::instruction::AddLiquidity2 {
        _liquidity_parameter: liquidity_parameter,
        _remaining_accounts_info: remaining_accounts_info,
    };
    let add_accounts = dlmm_idl::accounts::AddLiquidity2 {
        position: to_anchor_pubkey(position),
        lb_pair: to_anchor_pubkey(plan.lb_pair),
        bin_array_bitmap_extension: to_anchor_pubkey(plan.bin_array_bitmap_extension),
        user_token_x: to_anchor_pubkey(plan.user_token_x),
        user_token_y: to_anchor_pubkey(plan.user_token_y),
        reserve_x: to_anchor_pubkey(plan.reserve_x),
        reserve_y: to_anchor_pubkey(plan.reserve_y),
        token_x_mint: to_anchor_pubkey(plan.token_mint_x),
        token_y_mint: to_anchor_pubkey(plan.token_mint_y),
        sender: to_anchor_pubkey(plan.payer),
        token_x_program: to_anchor_pubkey(plan.token_program_x),
        token_y_program: to_anchor_pubkey(plan.token_program_y),
        event_authority: to_anchor_pubkey(event_authority),
        program: to_anchor_pubkey(METEORA_DLMM_ID),
    };
    let mut add_account_metas = to_solana_account_metas(add_accounts.to_account_metas(None));
    for (_idx, bin_array) in bin_arrays.iter().copied() {
        add_account_metas.push(AccountMeta::new(bin_array, false));
    }
    instructions.push(Instruction {
        program_id: METEORA_DLMM_ID,
        accounts: add_account_metas,
        data: add_ix.data(),
    });

    Ok(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer],
        derived,
        priority_fee_addresses: vec![
            METEORA_DLMM_ID,
            plan.token_program_x,
            plan.token_program_y,
            SYSTEM_PROGRAM,
            plan.lb_pair,
            position,
        ],
        instructions,
    })
}

#[derive(Debug, Clone)]
pub struct MeteoraDammV1CreatePoolPlan {
    pub payer: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub trade_fee_bps: u64,
    pub token_a_amount: u64,
    pub token_b_amount: u64,
    pub init_vault_a: bool,
    pub init_vault_b: bool,
    pub a_vault_lp_mint: Pubkey,
    pub b_vault_lp_mint: Pubkey,
}

pub fn plan_meteora_damm_v1_create_pool(
    plan: MeteoraDammV1CreatePoolPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedPoolTx> {
    let program_id = crate::dex::meteora_damm_v1::METEORA_DAMM_V1_ID;
    let vault_program_id = crate::dex::meteora_damm_v1::METEORA_DYNAMIC_VAULT_ID;
    let vault_base = crate::dex::meteora_damm_v1::METEORA_DYNAMIC_VAULT_BASE_ID;

    anyhow::ensure!(
        plan.token_a_mint != plan.token_b_mint,
        "token_a_mint must differ from token_b_mint"
    );
    anyhow::ensure!(
        plan.token_a_amount > 0 && plan.token_b_amount > 0,
        "token amounts must be > 0"
    );

    fn first_key(a: Pubkey, b: Pubkey) -> Pubkey {
        if a > b { a } else { b }
    }

    fn second_key(a: Pubkey, b: Pubkey) -> Pubkey {
        if a > b { b } else { a }
    }

    fn trade_fee_seed_bytes(trade_fee_bps: u64) -> Vec<u8> {
        // Match upstream SDK logic: omit fee tier seed when using default 25 bps.
        if trade_fee_bps == 25 {
            Vec::new()
        } else {
            trade_fee_bps.to_le_bytes().to_vec()
        }
    }

    let curve_type = 0u8; // ConstantProduct
    let trade_fee_seed = trade_fee_seed_bytes(plan.trade_fee_bps);
    let pool = Pubkey::find_program_address(
        &[
            &[curve_type],
            first_key(plan.token_a_mint, plan.token_b_mint).as_ref(),
            second_key(plan.token_a_mint, plan.token_b_mint).as_ref(),
            trade_fee_seed.as_ref(),
        ],
        &program_id,
    )
    .0;
    let lp_mint = Pubkey::find_program_address(&[b"lp_mint", pool.as_ref()], &program_id).0;
    let mint_metadata = Pubkey::find_program_address(
        &[
            b"metadata",
            crate::core::sol::METADATA_PROGRAM_ID.as_ref(),
            lp_mint.as_ref(),
        ],
        &crate::core::sol::METADATA_PROGRAM_ID,
    )
    .0;

    let vault_key = |mint: &Pubkey| -> Pubkey {
        Pubkey::find_program_address(
            &[b"vault", mint.as_ref(), vault_base.as_ref()],
            &vault_program_id,
        )
        .0
    };
    let token_vault_key = |vault: &Pubkey| -> Pubkey {
        Pubkey::find_program_address(&[b"token_vault", vault.as_ref()], &vault_program_id).0
    };

    let a_vault = vault_key(&plan.token_a_mint);
    let b_vault = vault_key(&plan.token_b_mint);
    let a_token_vault = token_vault_key(&a_vault);
    let b_token_vault = token_vault_key(&b_vault);
    let a_vault_lp_mint = plan.a_vault_lp_mint;
    let b_vault_lp_mint = plan.b_vault_lp_mint;

    let a_vault_lp =
        Pubkey::find_program_address(&[a_vault.as_ref(), pool.as_ref()], &program_id).0;
    let b_vault_lp =
        Pubkey::find_program_address(&[b_vault.as_ref(), pool.as_ref()], &program_id).0;
    let payer_token_a =
        derive_associated_token_address(&plan.payer, &plan.token_a_mint, &TOKEN_PROGRAM_ID);
    let payer_token_b =
        derive_associated_token_address(&plan.payer, &plan.token_b_mint, &TOKEN_PROGRAM_ID);
    let payer_pool_lp = derive_associated_token_address(&plan.payer, &lp_mint, &TOKEN_PROGRAM_ID);
    let protocol_token_a_fee = Pubkey::find_program_address(
        &[b"fee", plan.token_a_mint.as_ref(), pool.as_ref()],
        &program_id,
    )
    .0;
    let protocol_token_b_fee = Pubkey::find_program_address(
        &[b"fee", plan.token_b_mint.as_ref(), pool.as_ref()],
        &program_id,
    )
    .0;

    let mut instructions = Vec::new();

    if plan.init_vault_a {
        instructions.push(Instruction {
            program_id: vault_program_id,
            accounts: vec![
                AccountMeta::new(a_vault, false),
                AccountMeta::new(plan.payer, true),
                AccountMeta::new(a_token_vault, false),
                AccountMeta::new_readonly(plan.token_a_mint, false),
                AccountMeta::new(a_vault_lp_mint, false),
                AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            ],
            data: vec![
                175, 175, 109, 31, 13, 152, 155, 237, // dynamic_vault::initialize
            ],
        });
    }
    if plan.init_vault_b {
        instructions.push(Instruction {
            program_id: vault_program_id,
            accounts: vec![
                AccountMeta::new(b_vault, false),
                AccountMeta::new(plan.payer, true),
                AccountMeta::new(b_token_vault, false),
                AccountMeta::new_readonly(plan.token_b_mint, false),
                AccountMeta::new(b_vault_lp_mint, false),
                AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            ],
            data: vec![
                175, 175, 109, 31, 13, 152, 155, 237, // dynamic_vault::initialize
            ],
        });
    }

    // Ensure payer ATAs exist.
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_a_mint,
        &TOKEN_PROGRAM_ID,
    ));
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_b_mint,
        &TOKEN_PROGRAM_ID,
    ));

    // Wrap WSOL (if either side is WSOL).
    if plan.token_a_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &payer_token_a,
            plan.token_a_amount,
        ));
        instructions.push(sync_native(&TOKEN_PROGRAM_ID, &payer_token_a)?);
    }
    if plan.token_b_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &payer_token_b,
            plan.token_b_amount,
        ));
        instructions.push(sync_native(&TOKEN_PROGRAM_ID, &payer_token_b)?);
    }

    let mut data = Vec::with_capacity(8 + 1 + 8 + 8 + 8);
    data.extend_from_slice(&crate::dex::meteora_damm_v1::INITIALIZE_POOL_WITH_FEE_TIER_IX_DISCRIM);
    data.push(curve_type);
    data.extend_from_slice(&plan.trade_fee_bps.to_le_bytes());
    data.extend_from_slice(&plan.token_a_amount.to_le_bytes());
    data.extend_from_slice(&plan.token_b_amount.to_le_bytes());

    instructions.push(Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new(lp_mint, false),
            AccountMeta::new_readonly(plan.token_a_mint, false),
            AccountMeta::new_readonly(plan.token_b_mint, false),
            AccountMeta::new(a_vault, false),
            AccountMeta::new(b_vault, false),
            AccountMeta::new(a_token_vault, false),
            AccountMeta::new(b_token_vault, false),
            AccountMeta::new(a_vault_lp_mint, false),
            AccountMeta::new(b_vault_lp_mint, false),
            AccountMeta::new(a_vault_lp, false),
            AccountMeta::new(b_vault_lp, false),
            AccountMeta::new(payer_token_a, false),
            AccountMeta::new(payer_token_b, false),
            AccountMeta::new(payer_pool_lp, false),
            AccountMeta::new(protocol_token_a_fee, false),
            AccountMeta::new(protocol_token_b_fee, false),
            AccountMeta::new(plan.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
            AccountMeta::new(mint_metadata, false),
            AccountMeta::new_readonly(crate::core::sol::METADATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(vault_program_id, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ],
        data,
    });

    compute_budget.prepend_instructions(&mut instructions);

    Ok::<PlannedPoolTx, anyhow::Error>(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer],
        derived: DerivedAddresses::new()
            .insert("pool", pool)
            .insert("lp_mint", lp_mint)
            .insert("a_vault", a_vault)
            .insert("b_vault", b_vault)
            .insert("a_token_vault", a_token_vault)
            .insert("b_token_vault", b_token_vault)
            .insert("a_vault_lp_mint", a_vault_lp_mint)
            .insert("b_vault_lp_mint", b_vault_lp_mint)
            .insert("a_vault_lp", a_vault_lp)
            .insert("b_vault_lp", b_vault_lp)
            .insert("payer_token_a", payer_token_a)
            .insert("payer_token_b", payer_token_b)
            .insert("payer_pool_lp", payer_pool_lp)
            .insert("protocol_token_a_fee", protocol_token_a_fee)
            .insert("protocol_token_b_fee", protocol_token_b_fee)
            .insert("mint_metadata", mint_metadata),
        priority_fee_addresses: vec![
            program_id,
            vault_program_id,
            TOKEN_PROGRAM_ID,
            ATA_PROGRAM_ID,
            crate::core::sol::METADATA_PROGRAM_ID,
            SYSTEM_PROGRAM,
        ],
        instructions,
    })
    .context("plan meteora damm v1 create pool")
}

#[derive(Debug, Clone)]
pub struct RaydiumAmmV4CreatePoolPlan {
    pub payer: Pubkey,
    pub program_id: Pubkey,
    pub openbook_program_id: Pubkey,
    pub market: Pubkey,
    pub coin_mint: Pubkey,
    pub pc_mint: Pubkey,
    pub init_coin_amount: u64,
    pub init_pc_amount: u64,
    pub create_fee_destination: Pubkey,
}

pub fn plan_raydium_amm_v4_create_pool(
    plan: RaydiumAmmV4CreatePoolPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedPoolTx> {
    anyhow::ensure!(
        plan.coin_mint != plan.pc_mint,
        "coin_mint must differ from pc_mint"
    );
    anyhow::ensure!(
        plan.init_coin_amount > 0 && plan.init_pc_amount > 0,
        "init amounts must be > 0"
    );

    let (amm_authority, nonce) = Pubkey::find_program_address(
        &[crate::dex::raydium_amm_v4::RAYDIUM_AMM_V4_AUTHORITY_SEED],
        &plan.program_id,
    );
    let amm_config = Pubkey::find_program_address(
        &[crate::dex::raydium_amm_v4::AMM_CONFIG_SEED],
        &plan.program_id,
    )
    .0;

    let program_bytes = plan.program_id.to_bytes();
    let market_bytes = plan.market.to_bytes();
    let associated_for_market = |seed: &[u8]| -> Pubkey {
        Pubkey::find_program_address(
            &[program_bytes.as_ref(), market_bytes.as_ref(), seed],
            &plan.program_id,
        )
        .0
    };

    let amm = associated_for_market(crate::dex::raydium_amm_v4::AMM_ASSOCIATED_SEED);
    let amm_target_orders =
        associated_for_market(crate::dex::raydium_amm_v4::TARGET_ASSOCIATED_SEED);
    let amm_open_orders =
        associated_for_market(crate::dex::raydium_amm_v4::OPEN_ORDER_ASSOCIATED_SEED);
    let amm_lp_mint = associated_for_market(crate::dex::raydium_amm_v4::LP_MINT_ASSOCIATED_SEED);
    let amm_coin_vault =
        associated_for_market(crate::dex::raydium_amm_v4::COIN_VAULT_ASSOCIATED_SEED);
    let amm_pc_vault = associated_for_market(crate::dex::raydium_amm_v4::PC_VAULT_ASSOCIATED_SEED);

    let user_token_coin =
        derive_associated_token_address(&plan.payer, &plan.coin_mint, &TOKEN_PROGRAM_ID);
    let user_token_pc =
        derive_associated_token_address(&plan.payer, &plan.pc_mint, &TOKEN_PROGRAM_ID);
    let user_token_lp =
        derive_associated_token_address(&plan.payer, &amm_lp_mint, &TOKEN_PROGRAM_ID);

    let mut instructions = Vec::new();

    // Ensure user ATAs exist.
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.coin_mint,
        &TOKEN_PROGRAM_ID,
    ));
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.pc_mint,
        &TOKEN_PROGRAM_ID,
    ));

    // Wrap WSOL (if either side is WSOL).
    if plan.coin_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &user_token_coin,
            plan.init_coin_amount,
        ));
        instructions.push(sync_native(&TOKEN_PROGRAM_ID, &user_token_coin)?);
    }
    if plan.pc_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &user_token_pc,
            plan.init_pc_amount,
        ));
        instructions.push(sync_native(&TOKEN_PROGRAM_ID, &user_token_pc)?);
    }

    // raydium amm v4: initialize2
    let mut data = Vec::with_capacity(1 + 1 + 8 + 8 + 8);
    data.push(crate::dex::raydium_amm_v4::INITIALIZE2_IX_TAG);
    data.push(nonce);
    data.extend_from_slice(&0u64.to_le_bytes()); // open_time
    data.extend_from_slice(&plan.init_pc_amount.to_le_bytes());
    data.extend_from_slice(&plan.init_coin_amount.to_le_bytes());

    instructions.push(Instruction {
        program_id: plan.program_id,
        accounts: vec![
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(RENT_SYSVAR_ID, false),
            AccountMeta::new(amm, false),
            AccountMeta::new_readonly(amm_authority, false),
            AccountMeta::new(amm_open_orders, false),
            AccountMeta::new(amm_lp_mint, false),
            AccountMeta::new_readonly(plan.coin_mint, false),
            AccountMeta::new_readonly(plan.pc_mint, false),
            AccountMeta::new(amm_coin_vault, false),
            AccountMeta::new(amm_pc_vault, false),
            AccountMeta::new(amm_target_orders, false),
            AccountMeta::new_readonly(amm_config, false),
            AccountMeta::new(plan.create_fee_destination, false),
            AccountMeta::new_readonly(plan.openbook_program_id, false),
            AccountMeta::new_readonly(plan.market, false),
            AccountMeta::new(plan.payer, true),
            AccountMeta::new(user_token_coin, false),
            AccountMeta::new(user_token_pc, false),
            AccountMeta::new(user_token_lp, false),
        ],
        data,
    });

    compute_budget.prepend_instructions(&mut instructions);

    Ok::<PlannedPoolTx, anyhow::Error>(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer],
        derived: DerivedAddresses::new()
            .insert("amm_authority", amm_authority)
            .insert("amm_config", amm_config)
            .insert("amm", amm)
            .insert("amm_open_orders", amm_open_orders)
            .insert("amm_lp_mint", amm_lp_mint)
            .insert("amm_coin_vault", amm_coin_vault)
            .insert("amm_pc_vault", amm_pc_vault)
            .insert("amm_target_orders", amm_target_orders)
            .insert("user_token_coin", user_token_coin)
            .insert("user_token_pc", user_token_pc)
            .insert("user_token_lp", user_token_lp),
        priority_fee_addresses: vec![
            plan.program_id,
            plan.openbook_program_id,
            plan.market,
            TOKEN_PROGRAM_ID,
            ATA_PROGRAM_ID,
            SYSTEM_PROGRAM,
            plan.create_fee_destination,
        ],
        instructions,
    })
    .context("plan raydium amm v4 create pool")
}

#[derive(Debug, Clone)]
pub struct MeteoraDammV2CreatePoolPlan {
    pub payer: Pubkey,
    pub config: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub token_a_program: Pubkey,
    pub token_b_program: Pubkey,
    pub position_nft_mint: Pubkey,
    pub liquidity: u128,
    pub sqrt_price: u128,
    pub activation_point: Option<u64>,
    pub token_a_amount_in: u64,
    pub token_b_amount_in: u64,
    pub token_badge_a: Option<Pubkey>,
    pub token_badge_b: Option<Pubkey>,
}

pub fn plan_meteora_damm_v2_create_pool(
    plan: MeteoraDammV2CreatePoolPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedPoolTx> {
    let program_id = crate::dex::meteora_damm_v2::METEORA_DAMM_V2_ID;
    let pool_authority = crate::dex::meteora_damm_v2::METEORA_DAMM_V2_POOL_AUTHORITY;
    let event_authority = Pubkey::find_program_address(
        &[crate::dex::meteora_damm_v2::EVENT_AUTHORITY_SEED],
        &program_id,
    )
    .0;

    anyhow::ensure!(
        plan.token_a_mint != plan.token_b_mint,
        "token_a_mint must differ from token_b_mint"
    );
    anyhow::ensure!(plan.liquidity > 0, "liquidity must be > 0");
    anyhow::ensure!(plan.sqrt_price > 0, "sqrt_price must be > 0");

    let (max_mint, min_mint) = if plan.token_a_mint > plan.token_b_mint {
        (plan.token_a_mint, plan.token_b_mint)
    } else {
        (plan.token_b_mint, plan.token_a_mint)
    };

    let pool = Pubkey::find_program_address(
        &[
            crate::dex::meteora_damm_v2::POOL_PREFIX,
            plan.config.as_ref(),
            max_mint.as_ref(),
            min_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let position = Pubkey::find_program_address(
        &[
            crate::dex::meteora_damm_v2::POSITION_PREFIX,
            plan.position_nft_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let position_nft_account = Pubkey::find_program_address(
        &[
            crate::dex::meteora_damm_v2::POSITION_NFT_ACCOUNT_PREFIX,
            plan.position_nft_mint.as_ref(),
        ],
        &program_id,
    )
    .0;
    let token_a_vault = Pubkey::find_program_address(
        &[
            crate::dex::meteora_damm_v2::TOKEN_VAULT_PREFIX,
            plan.token_a_mint.as_ref(),
            pool.as_ref(),
        ],
        &program_id,
    )
    .0;
    let token_b_vault = Pubkey::find_program_address(
        &[
            crate::dex::meteora_damm_v2::TOKEN_VAULT_PREFIX,
            plan.token_b_mint.as_ref(),
            pool.as_ref(),
        ],
        &program_id,
    )
    .0;

    let payer_token_a =
        derive_associated_token_address(&plan.payer, &plan.token_a_mint, &plan.token_a_program);
    let payer_token_b =
        derive_associated_token_address(&plan.payer, &plan.token_b_mint, &plan.token_b_program);

    let mut instructions = Vec::new();

    // Ensure payer ATAs exist.
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_a_mint,
        &plan.token_a_program,
    ));
    instructions.push(create_associated_token_account_idempotent(
        &plan.payer,
        &plan.payer,
        &plan.token_b_mint,
        &plan.token_b_program,
    ));

    // Wrap WSOL (if either side is WSOL).
    if plan.token_a_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &payer_token_a,
            plan.token_a_amount_in,
        ));
        instructions.push(sync_native(&plan.token_a_program, &payer_token_a)?);
    }
    if plan.token_b_mint == WSOL_MINT {
        instructions.push(system_instruction_if::transfer(
            &plan.payer,
            &payer_token_b,
            plan.token_b_amount_in,
        ));
        instructions.push(sync_native(&plan.token_b_program, &payer_token_b)?);
    }

    let mut data = Vec::with_capacity(8 + 16 + 16 + 1 + 8);
    data.extend_from_slice(&crate::dex::meteora_damm_v2::INITIALIZE_POOL_IX_DISCRIM);
    data.extend_from_slice(&plan.liquidity.to_le_bytes());
    data.extend_from_slice(&plan.sqrt_price.to_le_bytes());
    match plan.activation_point {
        None => data.push(0u8),
        Some(point) => {
            data.push(1u8);
            data.extend_from_slice(&point.to_le_bytes());
        }
    }

    let mut accounts = vec![
        AccountMeta::new_readonly(plan.payer, false), // creator (unchecked)
        AccountMeta::new(plan.position_nft_mint, true),
        AccountMeta::new(position_nft_account, false),
        AccountMeta::new(plan.payer, true),
        AccountMeta::new_readonly(plan.config, false),
        AccountMeta::new_readonly(pool_authority, false),
        AccountMeta::new(pool, false),
        AccountMeta::new(position, false),
        AccountMeta::new_readonly(plan.token_a_mint, false),
        AccountMeta::new_readonly(plan.token_b_mint, false),
        AccountMeta::new(token_a_vault, false),
        AccountMeta::new(token_b_vault, false),
        AccountMeta::new(payer_token_a, false),
        AccountMeta::new(payer_token_b, false),
        AccountMeta::new_readonly(plan.token_a_program, false),
        AccountMeta::new_readonly(plan.token_b_program, false),
        AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
        AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(program_id, false),
    ];
    if let Some(badge) = plan.token_badge_a {
        accounts.push(AccountMeta::new_readonly(badge, false));
    }
    if let Some(badge) = plan.token_badge_b {
        accounts.push(AccountMeta::new_readonly(badge, false));
    }

    instructions.push(Instruction {
        program_id,
        accounts,
        data,
    });

    compute_budget.prepend_instructions(&mut instructions);

    Ok::<PlannedPoolTx, anyhow::Error>(PlannedPoolTx {
        payer: plan.payer,
        required_signers: vec![plan.payer, plan.position_nft_mint],
        derived: DerivedAddresses::new()
            .insert("pool", pool)
            .insert("position", position)
            .insert("position_nft_mint", plan.position_nft_mint)
            .insert("position_nft_account", position_nft_account)
            .insert("token_a_vault", token_a_vault)
            .insert("token_b_vault", token_b_vault)
            .insert("payer_token_a", payer_token_a)
            .insert("payer_token_b", payer_token_b)
            .insert("pool_authority", pool_authority)
            .insert("event_authority", event_authority),
        priority_fee_addresses: vec![
            program_id,
            plan.config,
            plan.token_a_program,
            plan.token_b_program,
            TOKEN_2022_PROGRAM_ID,
            ATA_PROGRAM_ID,
            SYSTEM_PROGRAM,
        ],
        instructions,
    })
    .context("plan meteora damm v2 create pool")
}

#[derive(Debug, Clone)]
pub struct MeteoraDbcCreatePoolPlan {
    pub payer: Pubkey,
    pub creator: Pubkey,
    pub config: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub quote_token_program: Pubkey,
    pub base_token_program: Pubkey,
    pub name: String,
    pub symbol: String,
    pub uri: String,
}

pub fn plan_meteora_dbc_create_pool(
    plan: MeteoraDbcCreatePoolPlan,
    compute_budget: ComputeBudgetPlan,
) -> anyhow::Result<PlannedPoolTx> {
    fn encode_borsh_string(value: &str, out: &mut Vec<u8>) -> anyhow::Result<()> {
        let bytes = value.as_bytes();
        let len: u32 = bytes
            .len()
            .try_into()
            .context("borsh string length overflow")?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(bytes);
        Ok(())
    }

    let MeteoraDbcCreatePoolPlan {
        payer,
        creator,
        config,
        base_mint,
        quote_mint,
        quote_token_program,
        base_token_program,
        name,
        symbol,
        uri,
    } = plan;

    anyhow::ensure!(
        base_mint != quote_mint,
        "base_mint must differ from quote_mint"
    );
    anyhow::ensure!(
        base_token_program == TOKEN_PROGRAM_ID || base_token_program == TOKEN_2022_PROGRAM_ID,
        "base_token_program must be TOKEN_PROGRAM_ID or TOKEN_2022_PROGRAM_ID"
    );

    let pool_authority_derived =
        Pubkey::find_program_address(&[METEORA_DBC_POOL_AUTHORITY_SEED], &METEORA_DBC_ID).0;
    anyhow::ensure!(
        pool_authority_derived == METEORA_DBC_POOL_AUTHORITY,
        "meteora dbc pool authority constant mismatch"
    );
    let event_authority =
        Pubkey::find_program_address(&[METEORA_DBC_EVENT_AUTHORITY_SEED], &METEORA_DBC_ID).0;

    let (max_mint, min_mint) = if base_mint > quote_mint {
        (base_mint, quote_mint)
    } else {
        (quote_mint, base_mint)
    };
    let pool = Pubkey::find_program_address(
        &[
            METEORA_DBC_POOL_PREFIX,
            config.as_ref(),
            max_mint.as_ref(),
            min_mint.as_ref(),
        ],
        &METEORA_DBC_ID,
    )
    .0;
    let base_vault = Pubkey::find_program_address(
        &[
            METEORA_DBC_TOKEN_VAULT_PREFIX,
            base_mint.as_ref(),
            pool.as_ref(),
        ],
        &METEORA_DBC_ID,
    )
    .0;
    let quote_vault = Pubkey::find_program_address(
        &[
            METEORA_DBC_TOKEN_VAULT_PREFIX,
            quote_mint.as_ref(),
            pool.as_ref(),
        ],
        &METEORA_DBC_ID,
    )
    .0;

    let mut derived = DerivedAddresses::new()
        .insert("pool", pool)
        .insert("base_vault", base_vault)
        .insert("quote_vault", quote_vault)
        .insert("pool_authority", METEORA_DBC_POOL_AUTHORITY)
        .insert("event_authority", event_authority);

    let mut required_signers = vec![payer, base_mint];
    if creator != payer && !required_signers.contains(&creator) {
        required_signers.push(creator);
    }

    let mut instructions = Vec::new();

    if base_token_program == TOKEN_PROGRAM_ID {
        let mint_metadata = derive_metaplex_metadata_pda(&base_mint);
        derived = derived.insert("mint_metadata", mint_metadata);

        let mut data = Vec::with_capacity(8 + 12 + name.len() + symbol.len() + uri.len());
        data.extend_from_slice(
            &crate::dex::meteora_dbc::INITIALIZE_VIRTUAL_POOL_WITH_SPL_TOKEN_IX_DISCRIM,
        );
        encode_borsh_string(&name, &mut data)?;
        encode_borsh_string(&symbol, &mut data)?;
        encode_borsh_string(&uri, &mut data)?;

        let accounts = vec![
            AccountMeta::new_readonly(config, false),
            AccountMeta::new_readonly(METEORA_DBC_POOL_AUTHORITY, false),
            AccountMeta::new_readonly(creator, true),
            AccountMeta::new(base_mint, true),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new(pool, false),
            AccountMeta::new(base_vault, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new(mint_metadata, false),
            AccountMeta::new_readonly(METADATA_PROGRAM_ID, false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(quote_token_program, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DBC_ID, false),
        ];
        instructions.push(Instruction {
            program_id: METEORA_DBC_ID,
            accounts,
            data,
        });
    } else {
        let mut data = Vec::with_capacity(8 + 12 + name.len() + symbol.len() + uri.len());
        data.extend_from_slice(
            &crate::dex::meteora_dbc::INITIALIZE_VIRTUAL_POOL_WITH_TOKEN_2022_IX_DISCRIM,
        );
        encode_borsh_string(&name, &mut data)?;
        encode_borsh_string(&symbol, &mut data)?;
        encode_borsh_string(&uri, &mut data)?;

        let accounts = vec![
            AccountMeta::new_readonly(config, false),
            AccountMeta::new_readonly(METEORA_DBC_POOL_AUTHORITY, false),
            AccountMeta::new_readonly(creator, true),
            AccountMeta::new(base_mint, true),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new(pool, false),
            AccountMeta::new(base_vault, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(quote_token_program, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DBC_ID, false),
        ];
        instructions.push(Instruction {
            program_id: METEORA_DBC_ID,
            accounts,
            data,
        });
    }

    compute_budget.prepend_instructions(&mut instructions);

    Ok::<PlannedPoolTx, anyhow::Error>(PlannedPoolTx {
        payer,
        required_signers,
        derived,
        priority_fee_addresses: vec![
            METEORA_DBC_ID,
            config,
            quote_token_program,
            base_token_program,
            TOKEN_2022_PROGRAM_ID,
            TOKEN_PROGRAM_ID,
            METADATA_PROGRAM_ID,
            SYSTEM_PROGRAM,
        ],
        instructions,
    })
    .context("plan meteora dbc create pool")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_raydium_clmm_seed_liquidity_tick_array_pdas_use_be_start_index() {
        let payer = Pubkey::new_from_array([41u8; 32]);
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let pool_state = Pubkey::new_from_array([43u8; 32]);
        let base_mint = Pubkey::new_from_array([44u8; 32]);
        let token_mint_0 = WSOL_MINT;
        let token_mint_1 = base_mint;
        let token_vault_0 = Pubkey::new_from_array([45u8; 32]);
        let token_vault_1 = Pubkey::new_from_array([46u8; 32]);
        let position_nft_mint = Pubkey::new_from_array([47u8; 32]);

        // sqrt_price_x64 = 1.0 * 2^64, so derived tick estimate centers around 0.
        let sqrt_price_x64 = 2u128.pow(64);
        let tick_spacing = 60u16;

        let planned = plan_raydium_clmm_seed_liquidity(RaydiumClmmSeedLiquidityPlan {
            payer,
            program_id,
            pool_state,
            token_mint_0,
            token_mint_1,
            token_program_0: TOKEN_PROGRAM_ID,
            token_program_1: TOKEN_PROGRAM_ID,
            token_vault_0,
            token_vault_1,
            position_nft_mint,
            base_mint,
            quote_mint: WSOL_MINT,
            base_amount_in: 1,
            quote_amount_in: 1,
            sqrt_price_x64,
            tick_spacing,
            with_metadata: false,
            open_position_base_is_mint0_override: None,
        })
        .expect("plan must succeed");

        let tick_array_lower = planned
            .derived
            .map
            .get("tick_array_lower")
            .copied()
            .expect("missing tick_array_lower");
        let tick_array_upper = planned
            .derived
            .map
            .get("tick_array_upper")
            .copied()
            .expect("missing tick_array_upper");

        // Derived from the plan's fixed default price range (~0.5x..2x around price=1),
        // floored/ceiled to tick-spacing, then compressed into tick arrays.
        let expected_lower_start: i32 = -7200;
        let expected_upper_start: i32 = 3600;

        let expected_lower = Pubkey::find_program_address(
            &[
                crate::dex::raydium_clmm::TICK_ARRAY_SEED,
                pool_state.as_ref(),
                &expected_lower_start.to_be_bytes(),
            ],
            &program_id,
        )
        .0;
        let expected_upper = Pubkey::find_program_address(
            &[
                crate::dex::raydium_clmm::TICK_ARRAY_SEED,
                pool_state.as_ref(),
                &expected_upper_start.to_be_bytes(),
            ],
            &program_id,
        )
        .0;

        assert_eq!(tick_array_lower, expected_lower);
        assert_eq!(tick_array_upper, expected_upper);
    }

    #[test]
    fn test_plan_raydium_amm_v4_create_pool_contract() {
        let payer = Pubkey::new_from_array([1u8; 32]);
        let program_id = Pubkey::new_from_array([2u8; 32]);
        let openbook_program_id = Pubkey::new_from_array([3u8; 32]);
        let market = Pubkey::new_from_array([4u8; 32]);
        let coin_mint = Pubkey::new_from_array([5u8; 32]);
        let pc_mint = Pubkey::new_from_array([6u8; 32]);
        let create_fee_destination = Pubkey::new_from_array([7u8; 32]);

        let planned = plan_raydium_amm_v4_create_pool(
            RaydiumAmmV4CreatePoolPlan {
                payer,
                program_id,
                openbook_program_id,
                market,
                coin_mint,
                pc_mint,
                init_coin_amount: 123,
                init_pc_amount: 456,
                create_fee_destination,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("plan must succeed");

        assert_eq!(planned.required_signers, vec![payer]);
        assert_eq!(planned.payer, payer);

        let (amm_authority, nonce) = Pubkey::find_program_address(
            &[crate::dex::raydium_amm_v4::RAYDIUM_AMM_V4_AUTHORITY_SEED],
            &program_id,
        );
        assert_eq!(
            planned.derived.map.get("amm_authority").copied(),
            Some(amm_authority)
        );

        // The last instruction is the Raydium AMM v4 initialize2.
        let init_ix = planned
            .instructions
            .last()
            .expect("must have initialize ix");
        assert_eq!(init_ix.program_id, program_id);
        assert_eq!(
            init_ix.data.first().copied(),
            Some(crate::dex::raydium_amm_v4::INITIALIZE2_IX_TAG)
        );
        assert_eq!(init_ix.data.get(1).copied(), Some(nonce));
        assert_eq!(init_ix.data.len(), 26);
        assert_eq!(init_ix.accounts.len(), 21);
        assert_eq!(init_ix.accounts[15].pubkey, openbook_program_id);
        assert_eq!(init_ix.accounts[16].pubkey, market);
        assert_eq!(init_ix.accounts[17].pubkey, payer);
    }

    #[test]
    fn test_plan_meteora_damm_v2_create_pool_contract() {
        let payer = Pubkey::new_from_array([9u8; 32]);
        let config = Pubkey::new_from_array([10u8; 32]);
        let token_a_mint = Pubkey::new_from_array([11u8; 32]);
        let token_b_mint = Pubkey::new_from_array([12u8; 32]);
        let position_nft_mint = Pubkey::new_from_array([13u8; 32]);

        let planned = plan_meteora_damm_v2_create_pool(
            MeteoraDammV2CreatePoolPlan {
                payer,
                config,
                token_a_mint,
                token_b_mint,
                token_a_program: TOKEN_PROGRAM_ID,
                token_b_program: TOKEN_PROGRAM_ID,
                position_nft_mint,
                liquidity: 123,
                sqrt_price: 456,
                activation_point: None,
                token_a_amount_in: 1,
                token_b_amount_in: 1,
                token_badge_a: None,
                token_badge_b: None,
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("plan must succeed");

        assert_eq!(planned.payer, payer);
        assert_eq!(planned.required_signers, vec![payer, position_nft_mint]);

        let init_ix = planned
            .instructions
            .last()
            .expect("must have initialize ix");
        assert_eq!(
            init_ix.program_id,
            crate::dex::meteora_damm_v2::METEORA_DAMM_V2_ID
        );
        assert!(
            init_ix
                .data
                .starts_with(&crate::dex::meteora_damm_v2::INITIALIZE_POOL_IX_DISCRIM)
        );
        assert_eq!(init_ix.accounts.len(), 20);
        assert_eq!(init_ix.accounts[1].pubkey, position_nft_mint);
        assert!(init_ix.accounts[1].is_signer);
        assert_eq!(init_ix.accounts[3].pubkey, payer);
        assert!(init_ix.accounts[3].is_signer);
        assert_eq!(init_ix.accounts[4].pubkey, config);
    }

    #[test]
    fn test_plan_meteora_dbc_create_pool_contract_spl_token() {
        let payer = Pubkey::new_from_array([21u8; 32]);
        let config = Pubkey::new_from_array([22u8; 32]);
        let base_mint = Pubkey::new_from_array([23u8; 32]);
        let quote_mint = WSOL_MINT;

        let planned = plan_meteora_dbc_create_pool(
            MeteoraDbcCreatePoolPlan {
                payer,
                creator: payer,
                config,
                base_mint,
                quote_mint,
                quote_token_program: TOKEN_PROGRAM_ID,
                base_token_program: TOKEN_PROGRAM_ID,
                name: "Example".to_string(),
                symbol: "EXMPL".to_string(),
                uri: "https://example.invalid/meta.json".to_string(),
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("plan must succeed");

        assert_eq!(planned.payer, payer);
        assert_eq!(planned.required_signers, vec![payer, base_mint]);

        let max_mint = std::cmp::max(base_mint, quote_mint);
        let min_mint = std::cmp::min(base_mint, quote_mint);
        let (pool, _bump) = Pubkey::find_program_address(
            &[
                crate::dex::meteora_dbc::POOL_PREFIX,
                config.as_ref(),
                max_mint.as_ref(),
                min_mint.as_ref(),
            ],
            &METEORA_DBC_ID,
        );
        assert_eq!(planned.derived.map.get("pool").copied(), Some(pool));

        let init_ix = planned.instructions.last().expect("must have init ix");
        assert_eq!(init_ix.program_id, METEORA_DBC_ID);
        assert!(init_ix.data.starts_with(
            &crate::dex::meteora_dbc::INITIALIZE_VIRTUAL_POOL_WITH_SPL_TOKEN_IX_DISCRIM
        ));
    }

    #[test]
    fn test_plan_meteora_dbc_create_pool_contract_token_2022() {
        let payer = Pubkey::new_from_array([31u8; 32]);
        let config = Pubkey::new_from_array([32u8; 32]);
        let base_mint = Pubkey::new_from_array([33u8; 32]);
        let quote_mint = WSOL_MINT;

        let planned = plan_meteora_dbc_create_pool(
            MeteoraDbcCreatePoolPlan {
                payer,
                creator: payer,
                config,
                base_mint,
                quote_mint,
                quote_token_program: TOKEN_PROGRAM_ID,
                base_token_program: TOKEN_2022_PROGRAM_ID,
                name: "Example".to_string(),
                symbol: "EXMPL".to_string(),
                uri: "https://example.invalid/meta.json".to_string(),
            },
            ComputeBudgetPlan {
                compute_unit_price_micro_lamports: None,
                compute_unit_limit: None,
            },
        )
        .expect("plan must succeed");

        assert_eq!(planned.payer, payer);
        assert_eq!(planned.required_signers, vec![payer, base_mint]);

        let init_ix = planned.instructions.last().expect("must have init ix");
        assert_eq!(init_ix.program_id, METEORA_DBC_ID);
        assert!(init_ix.data.starts_with(
            &crate::dex::meteora_dbc::INITIALIZE_VIRTUAL_POOL_WITH_TOKEN_2022_IX_DISCRIM
        ));
    }
}
