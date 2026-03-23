use borsh::BorshDeserialize;
use nusantara_loader_program::LoaderInstruction;
use nusantara_vm::ProgramCache;

use crate::cost_schedule::{
    LOADER_CLOSE_COST, LOADER_DEPLOY_COST, LOADER_INITIALIZE_BUFFER_COST,
    LOADER_SET_AUTHORITY_COST, LOADER_UPGRADE_COST, LOADER_WRITE_COST,
};
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

mod authority;
mod buffer;
mod deploy;
mod upgrade;

#[tracing::instrument(skip_all, fields(program = "loader"))]
pub fn process_loader(
    accounts: &[u8],
    data: &[u8],
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
    _program_cache: &ProgramCache,
) -> Result<(), RuntimeError> {
    let instruction = LoaderInstruction::try_from_slice(data)
        .map_err(|e| RuntimeError::InvalidInstructionData(e.to_string()))?;

    match instruction {
        LoaderInstruction::InitializeBuffer => {
            ctx.consume_compute(LOADER_INITIALIZE_BUFFER_COST)?;
            buffer::process_initialize_buffer(accounts, ctx)
        }
        LoaderInstruction::Write { offset, data } => {
            ctx.consume_compute(LOADER_WRITE_COST + data.len() as u64)?;
            buffer::process_write(accounts, offset, &data, ctx)
        }
        LoaderInstruction::Deploy { max_data_len } => {
            ctx.consume_compute(LOADER_DEPLOY_COST)?;
            deploy::process_deploy(accounts, max_data_len, ctx, sysvars)?;
            // No cache invalidation needed: the program cache is keyed by
            // bytecode hash, so new bytecode naturally produces a new cache key.
            // The old entry ages out via LRU eviction.
            Ok(())
        }
        LoaderInstruction::Upgrade => {
            ctx.consume_compute(LOADER_UPGRADE_COST)?;
            upgrade::process_upgrade(accounts, ctx)?;
            // No cache invalidation needed: the program cache is keyed by
            // bytecode hash, so upgraded bytecode naturally uses a new cache key.
            Ok(())
        }
        LoaderInstruction::SetAuthority { new_authority } => {
            ctx.consume_compute(LOADER_SET_AUTHORITY_COST)?;
            authority::process_set_authority(accounts, new_authority, ctx)
        }
        LoaderInstruction::Close => {
            ctx.consume_compute(LOADER_CLOSE_COST)?;
            authority::process_close(accounts, ctx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::program::LOADER_PROGRAM_ID;
    use nusantara_core::{Account, Message};
    use nusantara_crypto::hash;
    use nusantara_loader_program::state::LoaderState;

    use crate::test_utils::{test_cache, test_sysvars};

    #[test]
    fn initialize_buffer_success() {
        let buffer = hash(b"buffer");
        let authority = hash(b"authority");
        let ix = nusantara_loader_program::initialize_buffer(&buffer, &authority);
        let msg = Message::new(&[ix], &authority).unwrap();
        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| (*k, Account::new(100_000, nusantara_crypto::Hash::zero())))
            .collect();
        let compiled = msg.instructions[0].accounts.clone();
        let data = msg.instructions[0].data.clone();
        let mut ctx = TransactionContext::new(accounts, msg, 0, 100_000);
        let sysvars = test_sysvars();
        let cache = test_cache();
        process_loader(&compiled, &data, &mut ctx, &sysvars, &cache).unwrap();

        // Find buffer account and verify state
        let buffer_idx = ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &buffer)
            .unwrap();
        let acc = ctx.get_account(buffer_idx).unwrap();
        assert_eq!(acc.account.owner, *LOADER_PROGRAM_ID);

        let state = LoaderState::from_account_data(&acc.account.data).unwrap();
        assert!(state.is_buffer());
        assert_eq!(state.authority(), Some(&authority));
    }

    #[test]
    fn write_to_buffer() {
        let buffer = hash(b"buffer");
        let authority = hash(b"authority");

        // First initialize
        let init_ix = nusantara_loader_program::initialize_buffer(&buffer, &authority);
        let write_ix =
            nusantara_loader_program::write(&buffer, &authority, 0, vec![0x00, 0x61, 0x73, 0x6d]);
        let msg = Message::new(&[init_ix, write_ix], &authority).unwrap();
        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| (*k, Account::new(100_000, nusantara_crypto::Hash::zero())))
            .collect();

        let sysvars = test_sysvars();
        let cache = test_cache();
        let mut ctx = TransactionContext::new(accounts, msg.clone(), 0, 200_000);

        // Execute both instructions
        for ix in &msg.instructions {
            let program_id = &msg.account_keys[ix.program_id_index as usize];
            assert_eq!(*program_id, *LOADER_PROGRAM_ID);
            process_loader(&ix.accounts, &ix.data, &mut ctx, &sysvars, &cache).unwrap();
        }

        // Verify buffer has the written data
        let buffer_idx = ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &buffer)
            .unwrap();
        let acc = ctx.get_account(buffer_idx).unwrap();
        // Data should contain header + written bytes
        assert!(acc.account.data.len() > 4);
    }
}
