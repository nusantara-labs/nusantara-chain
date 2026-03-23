use borsh::BorshDeserialize;
use nusantara_compute_budget_program::{
    ComputeBudget, ComputeBudgetInstruction, DEFAULT_COMPUTE_UNIT_LIMIT,
    DEFAULT_COMPUTE_UNIT_PRICE, DEFAULT_HEAP_SIZE, DEFAULT_LOADED_ACCOUNTS_DATA_SIZE_LIMIT,
    MAX_COMPUTE_UNIT_LIMIT, MAX_HEAP_SIZE, MAX_LOADED_ACCOUNTS_DATA_SIZE_LIMIT,
};
use nusantara_core::Message;
use nusantara_core::program::COMPUTE_BUDGET_PROGRAM_ID;

use crate::error::RuntimeError;

pub fn parse_compute_budget(message: &Message) -> Result<ComputeBudget, RuntimeError> {
    let mut compute_unit_limit = None;
    let mut compute_unit_price = None;
    let mut heap_size = None;
    let mut loaded_accounts_data_size_limit = None;

    for (ix_index, ix) in message.instructions.iter().enumerate() {
        let program_id = &message.account_keys[ix.program_id_index as usize];
        if program_id != &*COMPUTE_BUDGET_PROGRAM_ID {
            continue;
        }

        let instruction = ComputeBudgetInstruction::try_from_slice(&ix.data).map_err(|e| {
            RuntimeError::InvalidComputeBudget(format!(
                "instruction {ix_index}: failed to deserialize: {e}"
            ))
        })?;

        match instruction {
            ComputeBudgetInstruction::RequestUnitsDeprecated { .. } => {
                return Err(RuntimeError::InvalidComputeBudget(
                    "RequestUnitsDeprecated is no longer supported".to_string(),
                ));
            }
            ComputeBudgetInstruction::SetComputeUnitLimit(units) => {
                if compute_unit_limit.is_some() {
                    return Err(RuntimeError::InvalidComputeBudget(
                        "duplicate SetComputeUnitLimit".to_string(),
                    ));
                }
                compute_unit_limit = Some((units as u64).min(MAX_COMPUTE_UNIT_LIMIT));
            }
            ComputeBudgetInstruction::SetComputeUnitPrice(price) => {
                if compute_unit_price.is_some() {
                    return Err(RuntimeError::InvalidComputeBudget(
                        "duplicate SetComputeUnitPrice".to_string(),
                    ));
                }
                compute_unit_price = Some(price);
            }
            ComputeBudgetInstruction::RequestHeapFrame(bytes) => {
                if heap_size.is_some() {
                    return Err(RuntimeError::InvalidComputeBudget(
                        "duplicate RequestHeapFrame".to_string(),
                    ));
                }
                if bytes as u64 > MAX_HEAP_SIZE || !bytes.is_power_of_two() {
                    return Err(RuntimeError::InvalidComputeBudget(format!(
                        "invalid heap frame size: {bytes} (max: {MAX_HEAP_SIZE}, must be power of 2)"
                    )));
                }
                heap_size = Some(bytes);
            }
            ComputeBudgetInstruction::SetLoadedAccountsDataSizeLimit(limit) => {
                if loaded_accounts_data_size_limit.is_some() {
                    return Err(RuntimeError::InvalidComputeBudget(
                        "duplicate SetLoadedAccountsDataSizeLimit".to_string(),
                    ));
                }
                loaded_accounts_data_size_limit =
                    Some((limit as u64).min(MAX_LOADED_ACCOUNTS_DATA_SIZE_LIMIT) as u32);
            }
        }
    }

    Ok(ComputeBudget {
        compute_unit_limit: compute_unit_limit.unwrap_or(DEFAULT_COMPUTE_UNIT_LIMIT),
        compute_unit_price: compute_unit_price.unwrap_or(DEFAULT_COMPUTE_UNIT_PRICE),
        heap_size: heap_size.unwrap_or(DEFAULT_HEAP_SIZE as u32),
        loaded_accounts_data_size_limit: loaded_accounts_data_size_limit
            .unwrap_or(DEFAULT_LOADED_ACCOUNTS_DATA_SIZE_LIMIT as u32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_compute_budget_program::{
        request_heap_frame, set_compute_unit_limit, set_compute_unit_price,
    };
    use nusantara_core::instruction::Instruction;
    use nusantara_core::program::SYSTEM_PROGRAM_ID;
    use nusantara_crypto::hash;

    fn make_message(instructions: &[Instruction]) -> Message {
        let payer = hash(b"payer");
        Message::new(instructions, &payer).unwrap()
    }

    fn dummy_system_ix() -> Instruction {
        Instruction {
            program_id: *SYSTEM_PROGRAM_ID,
            accounts: vec![],
            data: borsh::to_vec(&nusantara_system_program::SystemInstruction::Transfer {
                lamports: 100,
            })
            .unwrap(),
        }
    }

    #[test]
    fn default_budget() {
        let msg = make_message(&[dummy_system_ix()]);
        let budget = parse_compute_budget(&msg).unwrap();
        assert_eq!(budget, ComputeBudget::default());
    }

    #[test]
    fn set_limit() {
        let msg = make_message(&[set_compute_unit_limit(500_000), dummy_system_ix()]);
        let budget = parse_compute_budget(&msg).unwrap();
        assert_eq!(budget.compute_unit_limit, 500_000);
    }

    #[test]
    fn set_price() {
        let msg = make_message(&[set_compute_unit_price(1000), dummy_system_ix()]);
        let budget = parse_compute_budget(&msg).unwrap();
        assert_eq!(budget.compute_unit_price, 1000);
    }

    #[test]
    fn heap_frame() {
        let msg = make_message(&[request_heap_frame(65536), dummy_system_ix()]);
        let budget = parse_compute_budget(&msg).unwrap();
        assert_eq!(budget.heap_size, 65536);
    }

    #[test]
    fn data_size_limit() {
        let ix = Instruction {
            program_id: *COMPUTE_BUDGET_PROGRAM_ID,
            accounts: vec![],
            data: borsh::to_vec(&ComputeBudgetInstruction::SetLoadedAccountsDataSizeLimit(
                131072,
            ))
            .unwrap(),
        };
        let msg = make_message(&[ix, dummy_system_ix()]);
        let budget = parse_compute_budget(&msg).unwrap();
        assert_eq!(budget.loaded_accounts_data_size_limit, 131072);
    }

    #[test]
    fn duplicate_limit_error() {
        let msg = make_message(&[
            set_compute_unit_limit(100_000),
            set_compute_unit_limit(200_000),
            dummy_system_ix(),
        ]);
        let err = parse_compute_budget(&msg).unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidComputeBudget(_)));
    }

    #[test]
    fn max_heap_error() {
        // 512KB > MAX_HEAP_SIZE (256KB)
        let ix = Instruction {
            program_id: *COMPUTE_BUDGET_PROGRAM_ID,
            accounts: vec![],
            data: borsh::to_vec(&ComputeBudgetInstruction::RequestHeapFrame(524288)).unwrap(),
        };
        let msg = make_message(&[ix, dummy_system_ix()]);
        let err = parse_compute_budget(&msg).unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidComputeBudget(_)));
    }

    #[test]
    fn deprecated_error() {
        let ix = Instruction {
            program_id: *COMPUTE_BUDGET_PROGRAM_ID,
            accounts: vec![],
            data: borsh::to_vec(&ComputeBudgetInstruction::RequestUnitsDeprecated {
                units: 100,
                additional_fee: 10,
            })
            .unwrap(),
        };
        let msg = make_message(&[ix, dummy_system_ix()]);
        let err = parse_compute_budget(&msg).unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidComputeBudget(_)));
    }
}
