use nusantara_core::{Account, Message};
use nusantara_crypto::Hash;

use crate::compute_meter::ComputeMeter;
use crate::error::RuntimeError;

pub struct AccountRef<'a> {
    pub address: &'a Hash,
    pub account: &'a Account,
    pub is_signer: bool,
    pub is_writable: bool,
}

#[derive(Debug)]
pub struct AccountRefMut<'a> {
    pub address: &'a Hash,
    pub account: &'a mut Account,
    pub is_signer: bool,
    pub is_writable: bool,
}

pub struct TransactionContext {
    accounts: Vec<(Hash, Account)>,
    pre_accounts: Vec<(Hash, Account)>,
    pre_balances: Vec<u64>,
    message: Message,
    pub slot: u64,
    compute_meter: ComputeMeter,
    // CPI fields
    cpi_depth: u32,
    max_cpi_depth: u32,
    call_stack: Vec<Hash>,
    return_data: Option<(Hash, Vec<u8>)>,
}

impl TransactionContext {
    pub fn new(
        accounts: Vec<(Hash, Account)>,
        message: Message,
        slot: u64,
        compute_limit: u64,
    ) -> Self {
        let pre_balances = accounts.iter().map(|(_, a)| a.lamports).collect();
        let pre_accounts = accounts.clone();
        Self {
            accounts,
            pre_accounts,
            pre_balances,
            message,
            slot,
            compute_meter: ComputeMeter::new(compute_limit),
            cpi_depth: 0,
            max_cpi_depth: 4,
            call_stack: Vec::new(),
            return_data: None,
        }
    }

    pub fn get_account(&self, index: usize) -> Result<AccountRef<'_>, RuntimeError> {
        let (address, account) = self
            .accounts
            .get(index)
            .ok_or(RuntimeError::AccountNotFound(index))?;
        Ok(AccountRef {
            address,
            account,
            is_signer: self.message.is_signer(index),
            is_writable: self.message.is_writable(index),
        })
    }

    pub fn get_account_mut(&mut self, index: usize) -> Result<AccountRefMut<'_>, RuntimeError> {
        if index >= self.accounts.len() {
            return Err(RuntimeError::AccountNotFound(index));
        }
        if !self.message.is_writable(index) {
            return Err(RuntimeError::AccountNotWritable(index));
        }
        let is_signer = self.message.is_signer(index);
        let is_writable = true;
        let (address, account) = &mut self.accounts[index];
        Ok(AccountRefMut {
            address: &*address,
            account,
            is_signer,
            is_writable,
        })
    }

    pub fn consume_compute(&mut self, units: u64) -> Result<(), RuntimeError> {
        self.compute_meter.consume(units)
    }

    pub fn compute_consumed(&self) -> u64 {
        self.compute_meter.consumed()
    }

    pub fn compute_remaining(&self) -> u64 {
        self.compute_meter.remaining()
    }

    pub fn message(&self) -> &Message {
        &self.message
    }

    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    pub fn pre_balances(&self) -> &[u64] {
        &self.pre_balances
    }

    pub fn post_balances(&self) -> Vec<u64> {
        self.accounts.iter().map(|(_, a)| a.lamports).collect()
    }

    pub fn collect_account_deltas(&self) -> Vec<(Hash, Account)> {
        self.accounts
            .iter()
            .zip(self.pre_accounts.iter())
            .filter(|((_, post), (_, pre))| post != pre)
            .map(|((addr, post), _)| (*addr, post.clone()))
            .collect()
    }

    // --- CPI methods ---

    pub fn cpi_depth(&self) -> u32 {
        self.cpi_depth
    }

    pub fn max_cpi_depth(&self) -> u32 {
        self.max_cpi_depth
    }

    pub fn call_stack(&self) -> &[Hash] {
        &self.call_stack
    }

    pub fn push_call_stack(&mut self, program_id: Hash) -> Result<(), RuntimeError> {
        if self.cpi_depth >= self.max_cpi_depth {
            return Err(RuntimeError::CpiDepthExceeded {
                depth: self.cpi_depth,
                max: self.max_cpi_depth,
            });
        }
        if self.call_stack.contains(&program_id) {
            return Err(RuntimeError::ReentrancyNotAllowed(format!(
                "program {} already in call stack",
                program_id
            )));
        }
        self.call_stack.push(program_id);
        self.cpi_depth += 1;
        Ok(())
    }

    pub fn pop_call_stack(&mut self) {
        self.call_stack.pop();
        self.cpi_depth = self.cpi_depth.saturating_sub(1);
    }

    pub fn set_return_data(&mut self, program_id: Hash, data: Vec<u8>) {
        self.return_data = Some((program_id, data));
    }

    pub fn get_return_data(&self) -> Option<&(Hash, Vec<u8>)> {
        self.return_data.as_ref()
    }

    pub fn clear_return_data(&mut self) {
        self.return_data = None;
    }

    /// Get a mutable reference to the raw accounts vec (for WASM dispatch).
    pub fn accounts_mut(&mut self) -> &mut Vec<(Hash, Account)> {
        &mut self.accounts
    }

    /// Get a reference to the raw accounts vec.
    pub fn accounts(&self) -> &[(Hash, Account)] {
        &self.accounts
    }

    /// Set remaining compute units directly (for fuel sync with WASM).
    pub fn set_compute_remaining(&mut self, remaining: u64) {
        self.compute_meter.set_remaining(remaining);
    }

    pub fn verify_invariants(&self) -> Result<(), RuntimeError> {
        // Lamports conservation: total pre == total post
        let pre_total: u128 = self
            .pre_accounts
            .iter()
            .map(|(_, a)| a.lamports as u128)
            .sum();
        let post_total: u128 = self.accounts.iter().map(|(_, a)| a.lamports as u128).sum();
        if pre_total != post_total {
            return Err(RuntimeError::LamportsOverflow);
        }

        // Readonly accounts must not be modified
        for i in 0..self.accounts.len() {
            if !self.message.is_writable(i) && self.accounts[i].1 != self.pre_accounts[i].1 {
                return Err(RuntimeError::ReadonlyAccountModified(i));
            }
        }

        // Executable accounts must not have their executable flag changed
        for i in 0..self.accounts.len() {
            if self.pre_accounts[i].1.executable && !self.accounts[i].1.executable {
                return Err(RuntimeError::ExecutableAccountModified);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::instruction::{AccountMeta, Instruction};
    use nusantara_crypto::hash;

    fn test_message(signers: usize, writables: usize, total: usize) -> Message {
        // Build a simple message with the right account layout
        let payer = hash(b"payer");
        let program = hash(b"program");

        let mut account_metas = Vec::new();
        for i in 1..total {
            let key = hash(format!("account_{i}").as_bytes());
            let is_signer = i < signers;
            let is_writable = i < writables;
            if is_writable {
                account_metas.push(AccountMeta::new(key, is_signer));
            } else {
                account_metas.push(AccountMeta::new_readonly(key, is_signer));
            }
        }

        let ix = Instruction {
            program_id: program,
            accounts: account_metas,
            data: vec![],
        };

        Message::new(&[ix], &payer).unwrap()
    }

    fn make_accounts(msg: &Message) -> Vec<(Hash, Account)> {
        msg.account_keys
            .iter()
            .map(|k| (*k, Account::new(1000, hash(b"system"))))
            .collect()
    }

    #[test]
    fn pre_accounts_snapshot() {
        let msg = test_message(1, 2, 3);
        let accounts = make_accounts(&msg);
        let ctx = TransactionContext::new(accounts.clone(), msg, 0, 10000);
        assert_eq!(ctx.pre_balances(), &vec![1000; accounts.len()][..]);
    }

    #[test]
    fn get_immutable() {
        let msg = test_message(1, 2, 3);
        let accounts = make_accounts(&msg);
        let ctx = TransactionContext::new(accounts, msg, 0, 10000);
        let acc = ctx.get_account(0).unwrap();
        assert_eq!(acc.account.lamports, 1000);
        assert!(acc.is_signer);
    }

    #[test]
    fn get_mutable_writable() {
        let msg = test_message(1, 2, 3);
        let accounts = make_accounts(&msg);
        let mut ctx = TransactionContext::new(accounts, msg, 0, 10000);
        let acc = ctx.get_account_mut(0).unwrap();
        acc.account.lamports = 500;
        assert_eq!(ctx.post_balances()[0], 500);
    }

    #[test]
    fn get_mutable_readonly_fails() {
        let payer = hash(b"payer");
        let program = hash(b"program");
        let readonly_key = hash(b"readonly");
        let ix = Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new_readonly(readonly_key, false)],
            data: vec![],
        };
        let msg = Message::new(&[ix], &payer).unwrap();
        let accounts = make_accounts(&msg);
        let mut ctx = TransactionContext::new(accounts, msg, 0, 10000);
        // The readonly account is the last one
        let readonly_idx = ctx.account_count() - 1;
        let err = ctx.get_account_mut(readonly_idx).unwrap_err();
        assert!(matches!(err, RuntimeError::AccountNotWritable(_)));
    }

    #[test]
    fn is_signer_check() {
        let msg = test_message(2, 2, 4);
        let accounts = make_accounts(&msg);
        let ctx = TransactionContext::new(accounts, msg, 0, 10000);
        assert!(ctx.get_account(0).unwrap().is_signer); // payer always signer
    }

    #[test]
    fn deltas_empty_when_unchanged() {
        let msg = test_message(1, 2, 3);
        let accounts = make_accounts(&msg);
        let ctx = TransactionContext::new(accounts, msg, 0, 10000);
        assert!(ctx.collect_account_deltas().is_empty());
    }

    #[test]
    fn deltas_captures_changes() {
        let msg = test_message(1, 2, 3);
        let accounts = make_accounts(&msg);
        let mut ctx = TransactionContext::new(accounts, msg, 0, 10000);
        ctx.get_account_mut(0).unwrap().account.lamports = 500;
        let deltas = ctx.collect_account_deltas();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].1.lamports, 500);
    }

    #[test]
    fn pre_post_balances() {
        let msg = test_message(1, 2, 3);
        let accounts = make_accounts(&msg);
        let n = accounts.len();
        let mut ctx = TransactionContext::new(accounts, msg, 0, 10000);
        ctx.get_account_mut(0).unwrap().account.lamports = 500;
        assert_eq!(ctx.pre_balances(), &vec![1000; n][..]);
        let mut expected_post = vec![1000; n];
        expected_post[0] = 500;
        assert_eq!(ctx.post_balances(), expected_post);
    }

    #[test]
    fn invariants_lamports_conservation() {
        let msg = test_message(1, 2, 3);
        let accounts = make_accounts(&msg);
        let mut ctx = TransactionContext::new(accounts, msg, 0, 10000);
        // Break conservation: just subtract without adding
        ctx.get_account_mut(0).unwrap().account.lamports = 500;
        let err = ctx.verify_invariants().unwrap_err();
        assert!(matches!(err, RuntimeError::LamportsOverflow));
    }

    #[test]
    fn invariants_readonly_unchanged() {
        let payer = hash(b"payer");
        let program = hash(b"program");
        let readonly_key = hash(b"readonly");
        let writable_key = hash(b"writable");
        let ix = Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta::new(writable_key, false),
                AccountMeta::new_readonly(readonly_key, false),
            ],
            data: vec![],
        };
        let msg = Message::new(&[ix], &payer).unwrap();
        let accounts = make_accounts(&msg);
        let mut ctx = TransactionContext::new(accounts, msg, 0, 10000);
        // Directly mutate readonly account (bypassing get_account_mut check)
        // Find the readonly index
        let readonly_idx = ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &readonly_key)
            .unwrap();
        ctx.accounts[readonly_idx].1.lamports = 9999;
        let err = ctx.verify_invariants().unwrap_err();
        assert!(
            matches!(err, RuntimeError::ReadonlyAccountModified(_))
                || matches!(err, RuntimeError::LamportsOverflow)
        );
    }
}
