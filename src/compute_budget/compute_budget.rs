use solana_program::{instruction::Instruction, pubkey::Pubkey};

pub const COMPUTE_BUDGET_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");

pub fn ix_set_compute_unit_price(micro_lamports_per_cu: u64) -> Instruction {
    let mut data = Vec::with_capacity(1 + 8);
    data.push(3u8); // tag for SetComputeUnitPrice
    data.extend_from_slice(&micro_lamports_per_cu.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}

pub fn ix_set_compute_unit_limit(units: u32) -> Instruction {
    let mut data = Vec::with_capacity(1 + 4);
    data.push(2u8); // tag for SetComputeUnitLimit
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}
