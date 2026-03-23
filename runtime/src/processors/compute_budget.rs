use crate::error::RuntimeError;
use crate::transaction_context::TransactionContext;

pub fn process_compute_budget(
    _accounts: &[u8],
    _data: &[u8],
    _ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    // No-op: compute budget instructions are pre-parsed before execution
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::instruction::Instruction;
    use nusantara_core::program::COMPUTE_BUDGET_PROGRAM_ID;
    use nusantara_core::{Account, Message};
    use nusantara_crypto::hash;

    #[test]
    fn process_is_noop() {
        let payer = hash(b"payer");
        let ix = Instruction {
            program_id: *COMPUTE_BUDGET_PROGRAM_ID,
            accounts: vec![],
            data: vec![1, 2, 3],
        };
        let msg = Message::new(&[ix], &payer).unwrap();
        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| (*k, Account::new(1000, hash(b"system"))))
            .collect();
        let mut ctx = TransactionContext::new(accounts, msg, 0, 10000);
        process_compute_budget(&[], &[1, 2, 3], &mut ctx).unwrap();
    }
}
