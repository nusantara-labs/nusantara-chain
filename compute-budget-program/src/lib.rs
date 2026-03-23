use borsh::{BorshDeserialize, BorshSerialize};

use nusantara_core::instruction::Instruction;
use nusantara_core::native_token::const_parse_u64;
use nusantara_core::program::COMPUTE_BUDGET_PROGRAM_ID;

pub const DEFAULT_COMPUTE_UNIT_LIMIT: u64 =
    const_parse_u64(env!("NUSA_COMPUTE_BUDGET_DEFAULT_COMPUTE_UNIT_LIMIT"));
pub const MAX_COMPUTE_UNIT_LIMIT: u64 =
    const_parse_u64(env!("NUSA_COMPUTE_BUDGET_MAX_COMPUTE_UNIT_LIMIT"));
pub const DEFAULT_COMPUTE_UNIT_PRICE: u64 =
    const_parse_u64(env!("NUSA_COMPUTE_BUDGET_DEFAULT_COMPUTE_UNIT_PRICE"));
pub const DEFAULT_HEAP_SIZE: u64 =
    const_parse_u64(env!("NUSA_COMPUTE_BUDGET_DEFAULT_HEAP_SIZE"));
pub const MAX_HEAP_SIZE: u64 = const_parse_u64(env!("NUSA_COMPUTE_BUDGET_MAX_HEAP_SIZE"));
pub const DEFAULT_LOADED_ACCOUNTS_DATA_SIZE_LIMIT: u64 =
    const_parse_u64(env!("NUSA_COMPUTE_BUDGET_DEFAULT_LOADED_ACCOUNTS_DATA_SIZE_LIMIT"));
pub const MAX_LOADED_ACCOUNTS_DATA_SIZE_LIMIT: u64 =
    const_parse_u64(env!("NUSA_COMPUTE_BUDGET_MAX_LOADED_ACCOUNTS_DATA_SIZE_LIMIT"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ComputeBudget {
    pub compute_unit_limit: u64,
    pub compute_unit_price: u64,
    pub heap_size: u32,
    pub loaded_accounts_data_size_limit: u32,
}

impl Default for ComputeBudget {
    fn default() -> Self {
        Self {
            compute_unit_limit: DEFAULT_COMPUTE_UNIT_LIMIT,
            compute_unit_price: DEFAULT_COMPUTE_UNIT_PRICE,
            heap_size: DEFAULT_HEAP_SIZE as u32,
            loaded_accounts_data_size_limit: DEFAULT_LOADED_ACCOUNTS_DATA_SIZE_LIMIT as u32,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ComputeBudgetInstruction {
    RequestUnitsDeprecated { units: u32, additional_fee: u32 },
    RequestHeapFrame(u32),
    SetComputeUnitLimit(u32),
    SetComputeUnitPrice(u64),
    SetLoadedAccountsDataSizeLimit(u32),
}

pub fn request_heap_frame(bytes: u32) -> Instruction {
    let data = borsh::to_vec(&ComputeBudgetInstruction::RequestHeapFrame(bytes))
        .expect("serialization cannot fail");

    Instruction {
        program_id: *COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}

pub fn set_compute_unit_limit(units: u32) -> Instruction {
    let data = borsh::to_vec(&ComputeBudgetInstruction::SetComputeUnitLimit(units))
        .expect("serialization cannot fail");

    Instruction {
        program_id: *COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}

pub fn set_compute_unit_price(micro_lamports: u64) -> Instruction {
    let data = borsh::to_vec(&ComputeBudgetInstruction::SetComputeUnitPrice(micro_lamports))
        .expect("serialization cannot fail");

    Instruction {
        program_id: *COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_compute_budget_values() {
        assert_eq!(DEFAULT_COMPUTE_UNIT_LIMIT, 200_000);
        assert_eq!(MAX_COMPUTE_UNIT_LIMIT, 1_400_000);
        assert_eq!(DEFAULT_COMPUTE_UNIT_PRICE, 0);
        assert_eq!(DEFAULT_HEAP_SIZE, 32768);
        assert_eq!(MAX_HEAP_SIZE, 262144);
    }

    #[test]
    fn compute_budget_instruction_borsh_roundtrip() {
        let instructions = vec![
            ComputeBudgetInstruction::RequestUnitsDeprecated {
                units: 100,
                additional_fee: 10,
            },
            ComputeBudgetInstruction::RequestHeapFrame(65536),
            ComputeBudgetInstruction::SetComputeUnitLimit(400_000),
            ComputeBudgetInstruction::SetComputeUnitPrice(1000),
            ComputeBudgetInstruction::SetLoadedAccountsDataSizeLimit(131072),
        ];

        for ix in &instructions {
            let encoded = borsh::to_vec(ix).unwrap();
            let decoded: ComputeBudgetInstruction = borsh::from_slice(&encoded).unwrap();
            assert_eq!(*ix, decoded);
        }
    }

    #[test]
    fn builder_functions() {
        let ix = set_compute_unit_limit(500_000);
        assert_eq!(ix.program_id, *COMPUTE_BUDGET_PROGRAM_ID);
        let decoded: ComputeBudgetInstruction = borsh::from_slice(&ix.data).unwrap();
        assert_eq!(
            decoded,
            ComputeBudgetInstruction::SetComputeUnitLimit(500_000)
        );

        let ix = set_compute_unit_price(2000);
        let decoded: ComputeBudgetInstruction = borsh::from_slice(&ix.data).unwrap();
        assert_eq!(
            decoded,
            ComputeBudgetInstruction::SetComputeUnitPrice(2000)
        );
    }
}
