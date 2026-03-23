//! Nusantara example: simple counter program.
//!
//! This program stores a `u64` counter in the first account's data.
//! Instructions:
//!   - `[0]` — Initialize counter to 0
//!   - `[1]` — Increment counter by 1
//!   - `[2, ...le_bytes...]` — Increment counter by a given u64 value

use nusantara_sdk::prelude::*;

entrypoint!(process_instruction);

fn process_instruction(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.is_empty() {
        msg!("Counter: no instruction data");
        return Err(ProgramError::InvalidInstructionData);
    }

    match data[0] {
        // Initialize
        0 => {
            msg!("Counter: initialize");
            Ok(())
        }
        // Increment by 1
        1 => {
            msg!("Counter: increment by 1");
            Ok(())
        }
        // Increment by value
        2 => {
            if data.len() < 9 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let value = u64::from_le_bytes(data[1..9].try_into().unwrap());
            msg!("Counter: increment by {}", value);
            Ok(())
        }
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
