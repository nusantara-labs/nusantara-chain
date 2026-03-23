mod account;
mod burn;
mod freeze;
mod mint;
mod transfer;

use nusantara_token_program::TokenInstruction;

use crate::cost_schedule::{
    TOKEN_APPROVE_COST, TOKEN_BURN_COST, TOKEN_CLOSE_COST, TOKEN_FREEZE_COST,
    TOKEN_INIT_ACCOUNT_COST, TOKEN_INIT_MINT_COST, TOKEN_MINT_TO_COST, TOKEN_REVOKE_COST,
    TOKEN_THAW_COST, TOKEN_TRANSFER_COST,
};
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

#[tracing::instrument(skip_all, fields(program = "token"))]
pub fn process_token(
    accounts: &[u8],
    data: &[u8],
    ctx: &mut TransactionContext,
    _sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    use borsh::BorshDeserialize;

    let instruction = TokenInstruction::try_from_slice(data)
        .map_err(|e| RuntimeError::InvalidInstructionData(e.to_string()))?;

    match instruction {
        TokenInstruction::InitializeMint {
            decimals,
            mint_authority,
            freeze_authority,
        } => {
            ctx.consume_compute(TOKEN_INIT_MINT_COST)?;
            mint::process_initialize_mint(accounts, ctx, decimals, mint_authority, freeze_authority)
        }
        TokenInstruction::InitializeAccount => {
            ctx.consume_compute(TOKEN_INIT_ACCOUNT_COST)?;
            account::process_initialize_account(accounts, ctx)
        }
        TokenInstruction::MintTo { amount } => {
            ctx.consume_compute(TOKEN_MINT_TO_COST)?;
            mint::process_mint_to(accounts, ctx, amount)
        }
        TokenInstruction::Transfer { amount } => {
            ctx.consume_compute(TOKEN_TRANSFER_COST)?;
            transfer::process_transfer(accounts, ctx, amount)
        }
        TokenInstruction::Approve { amount } => {
            ctx.consume_compute(TOKEN_APPROVE_COST)?;
            transfer::process_approve(accounts, ctx, amount)
        }
        TokenInstruction::Revoke => {
            ctx.consume_compute(TOKEN_REVOKE_COST)?;
            transfer::process_revoke(accounts, ctx)
        }
        TokenInstruction::Burn { amount } => {
            ctx.consume_compute(TOKEN_BURN_COST)?;
            burn::process_burn(accounts, ctx, amount)
        }
        TokenInstruction::CloseAccount => {
            ctx.consume_compute(TOKEN_CLOSE_COST)?;
            account::process_close_account(accounts, ctx)
        }
        TokenInstruction::FreezeAccount => {
            ctx.consume_compute(TOKEN_FREEZE_COST)?;
            freeze::process_freeze_account(accounts, ctx)
        }
        TokenInstruction::ThawAccount => {
            ctx.consume_compute(TOKEN_THAW_COST)?;
            freeze::process_thaw_account(accounts, ctx)
        }
    }
}

pub(super) fn token_err(e: nusantara_token_program::error::TokenError) -> RuntimeError {
    RuntimeError::ProgramError {
        program: "token".to_string(),
        message: e.to_string(),
    }
}

pub(super) fn borsh_err(e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::ProgramError {
        program: "token".to_string(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::{Account, Message};
    use nusantara_crypto::hash;
    use nusantara_token_program::state::{AccountState, Mint, TokenAccount};

    use crate::test_utils::test_sysvars;
    use crate::transaction_context::TransactionContext;

    #[test]
    fn mint_and_transfer() {
        let mint_addr = hash(b"mint");
        let owner = hash(b"owner");
        let alice = hash(b"alice");
        let bob = hash(b"bob");

        // Build a tx that initializes mint, then init two token accounts, mint, and transfer
        let ix_init_mint = nusantara_token_program::initialize_mint(&mint_addr, 9, &owner, None);
        let ix_init_alice = nusantara_token_program::initialize_account(&alice, &mint_addr, &owner);
        let ix_init_bob = nusantara_token_program::initialize_account(&bob, &mint_addr, &owner);
        let ix_mint = nusantara_token_program::mint_to(&mint_addr, &alice, &owner, 1000);
        let ix_transfer = nusantara_token_program::transfer(&alice, &bob, &owner, 400);

        let msg = Message::new(
            &[
                ix_init_mint,
                ix_init_alice,
                ix_init_bob,
                ix_mint,
                ix_transfer,
            ],
            &owner,
        )
        .unwrap();

        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| (*k, Account::new(100_000, nusantara_crypto::Hash::zero())))
            .collect();

        let mut ctx = TransactionContext::new(accounts, msg.clone(), 0, 1_000_000);
        let sysvars = test_sysvars();

        for ix in &msg.instructions {
            process_token(&ix.accounts, &ix.data, &mut ctx, &sysvars).unwrap();
        }

        // Find alice and bob indices
        let alice_idx = msg.account_keys.iter().position(|k| k == &alice).unwrap();
        let bob_idx = msg.account_keys.iter().position(|k| k == &bob).unwrap();
        let mint_idx = msg
            .account_keys
            .iter()
            .position(|k| k == &mint_addr)
            .unwrap();

        let alice_acc = ctx.get_account(alice_idx).unwrap();
        let alice_token: TokenAccount = borsh::from_slice(&alice_acc.account.data).unwrap();
        assert_eq!(alice_token.amount, 600);

        let bob_acc = ctx.get_account(bob_idx).unwrap();
        let bob_token: TokenAccount = borsh::from_slice(&bob_acc.account.data).unwrap();
        assert_eq!(bob_token.amount, 400);

        let mint_acc = ctx.get_account(mint_idx).unwrap();
        let mint: Mint = borsh::from_slice(&mint_acc.account.data).unwrap();
        assert_eq!(mint.supply, 1000);
    }

    #[test]
    fn burn_tokens() {
        let mint_addr = hash(b"mint");
        let owner = hash(b"owner");
        let alice = hash(b"alice");

        let ix_init_mint = nusantara_token_program::initialize_mint(&mint_addr, 9, &owner, None);
        let ix_init_alice = nusantara_token_program::initialize_account(&alice, &mint_addr, &owner);
        let ix_mint = nusantara_token_program::mint_to(&mint_addr, &alice, &owner, 1000);
        let ix_burn = nusantara_token_program::burn(&alice, &mint_addr, &owner, 300);

        let msg = Message::new(&[ix_init_mint, ix_init_alice, ix_mint, ix_burn], &owner).unwrap();

        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| (*k, Account::new(100_000, nusantara_crypto::Hash::zero())))
            .collect();

        let mut ctx = TransactionContext::new(accounts, msg.clone(), 0, 1_000_000);
        let sysvars = test_sysvars();

        for ix in &msg.instructions {
            process_token(&ix.accounts, &ix.data, &mut ctx, &sysvars).unwrap();
        }

        let alice_idx = msg.account_keys.iter().position(|k| k == &alice).unwrap();
        let alice_acc = ctx.get_account(alice_idx).unwrap();
        let alice_token: TokenAccount = borsh::from_slice(&alice_acc.account.data).unwrap();
        assert_eq!(alice_token.amount, 700);

        let mint_idx = msg
            .account_keys
            .iter()
            .position(|k| k == &mint_addr)
            .unwrap();
        let mint_acc = ctx.get_account(mint_idx).unwrap();
        let mint: Mint = borsh::from_slice(&mint_acc.account.data).unwrap();
        assert_eq!(mint.supply, 700);
    }

    #[test]
    fn freeze_and_thaw() {
        let mint_addr = hash(b"mint");
        let owner = hash(b"owner");
        let alice = hash(b"alice");

        let ix_init_mint =
            nusantara_token_program::initialize_mint(&mint_addr, 9, &owner, Some(&owner));
        let ix_init_alice = nusantara_token_program::initialize_account(&alice, &mint_addr, &owner);
        let ix_freeze = nusantara_token_program::freeze_account(&alice, &mint_addr, &owner);

        let msg = Message::new(&[ix_init_mint, ix_init_alice, ix_freeze], &owner).unwrap();

        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| (*k, Account::new(100_000, nusantara_crypto::Hash::zero())))
            .collect();

        let mut ctx = TransactionContext::new(accounts, msg.clone(), 0, 1_000_000);
        let sysvars = test_sysvars();

        for ix in &msg.instructions {
            process_token(&ix.accounts, &ix.data, &mut ctx, &sysvars).unwrap();
        }

        let alice_idx = msg.account_keys.iter().position(|k| k == &alice).unwrap();
        let alice_acc = ctx.get_account(alice_idx).unwrap();
        let alice_token: TokenAccount = borsh::from_slice(&alice_acc.account.data).unwrap();
        assert_eq!(alice_token.state, AccountState::Frozen);

        // Now thaw
        let ix_thaw = nusantara_token_program::thaw_account(&alice, &mint_addr, &owner);
        let msg2 = Message::new(&[ix_thaw], &owner).unwrap();
        let accounts2: Vec<_> = msg2
            .account_keys
            .iter()
            .map(|k| {
                // Carry over the data from ctx for alice and mint
                let idx = msg.account_keys.iter().position(|mk| mk == k);
                if let Some(i) = idx {
                    let a = ctx.get_account(i).unwrap();
                    (*k, a.account.clone())
                } else {
                    (*k, Account::new(100_000, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let mut ctx2 = TransactionContext::new(accounts2, msg2.clone(), 0, 1_000_000);
        for ix in &msg2.instructions {
            process_token(&ix.accounts, &ix.data, &mut ctx2, &sysvars).unwrap();
        }

        let alice_idx2 = msg2.account_keys.iter().position(|k| k == &alice).unwrap();
        let alice_acc2 = ctx2.get_account(alice_idx2).unwrap();
        let alice_token2: TokenAccount = borsh::from_slice(&alice_acc2.account.data).unwrap();
        assert_eq!(alice_token2.state, AccountState::Initialized);
    }
}
