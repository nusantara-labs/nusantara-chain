use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;

use crate::error::CoreError;
use crate::instruction::{CompiledInstruction, Instruction};

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Message {
    pub num_required_signatures: u8,
    pub num_readonly_signed: u8,
    pub num_readonly_unsigned: u8,
    pub account_keys: Vec<Hash>,
    pub recent_blockhash: Hash,
    pub instructions: Vec<CompiledInstruction>,
}

impl Message {
    pub fn new(instructions: &[Instruction], payer: &Hash) -> Result<Self, CoreError> {
        if instructions.is_empty() {
            return Err(CoreError::InvalidMessage(
                "no instructions provided".to_string(),
            ));
        }

        // Collect unique accounts preserving order: payer first
        let mut account_keys: Vec<Hash> = vec![*payer];
        let mut is_signer = vec![true];
        let mut is_writable = vec![true];

        for ix in instructions {
            // Add program_id
            if let Some(pos) = account_keys.iter().position(|k| k == &ix.program_id) {
                is_writable[pos] = false; // program accounts are not writable
            } else {
                account_keys.push(ix.program_id);
                is_signer.push(false);
                is_writable.push(false);
            }

            // Add instruction accounts
            for meta in &ix.accounts {
                if let Some(pos) = account_keys.iter().position(|k| k == &meta.pubkey) {
                    is_signer[pos] = is_signer[pos] || meta.is_signer;
                    is_writable[pos] = is_writable[pos] || meta.is_writable;
                } else {
                    account_keys.push(meta.pubkey);
                    is_signer.push(meta.is_signer);
                    is_writable.push(meta.is_writable);
                }
            }
        }

        // Count signature categories
        let num_required_signatures = is_signer.iter().filter(|&&s| s).count() as u8;
        let num_readonly_signed = is_signer
            .iter()
            .zip(is_writable.iter())
            .filter(|(s, w)| **s && !**w)
            .count() as u8;
        let num_readonly_unsigned = is_signer
            .iter()
            .zip(is_writable.iter())
            .filter(|(s, w)| !**s && !**w)
            .count() as u8;

        // Sort accounts: signed-writable, signed-readonly, unsigned-writable, unsigned-readonly
        // Payer (index 0) always stays first.
        let mut indices: Vec<usize> = (0..account_keys.len()).collect();
        indices[1..].sort_by(|&a, &b| {
            let order = |i: usize| -> u8 {
                match (is_signer[i], is_writable[i]) {
                    (true, true) => 0,
                    (true, false) => 1,
                    (false, true) => 2,
                    (false, false) => 3,
                }
            };
            order(a).cmp(&order(b))
        });

        let sorted_keys: Vec<Hash> = indices.iter().map(|&i| account_keys[i]).collect();

        // Build reverse mapping: old index -> new index
        let mut old_to_new = vec![0usize; account_keys.len()];
        for (new_idx, &old_idx) in indices.iter().enumerate() {
            old_to_new[old_idx] = new_idx;
        }

        // Compile instructions using sorted account positions
        let compiled_instructions = instructions
            .iter()
            .map(|ix| {
                let old_program_idx = account_keys
                    .iter()
                    .position(|k| k == &ix.program_id)
                    .unwrap();
                let program_id_index = old_to_new[old_program_idx] as u8;
                let accounts = ix
                    .accounts
                    .iter()
                    .map(|meta| {
                        let old_idx = account_keys
                            .iter()
                            .position(|k| k == &meta.pubkey)
                            .unwrap();
                        old_to_new[old_idx] as u8
                    })
                    .collect();
                CompiledInstruction {
                    program_id_index,
                    accounts,
                    data: ix.data.clone(),
                }
            })
            .collect();

        Ok(Self {
            num_required_signatures,
            num_readonly_signed,
            num_readonly_unsigned,
            account_keys: sorted_keys,
            recent_blockhash: Hash::zero(),
            instructions: compiled_instructions,
        })
    }

    pub fn program_id(&self, ix_index: usize) -> Option<&Hash> {
        self.instructions
            .get(ix_index)
            .and_then(|ix| self.account_keys.get(ix.program_id_index as usize))
    }

    pub fn is_signer(&self, index: usize) -> bool {
        index < self.num_required_signatures as usize
    }

    pub fn is_writable(&self, index: usize) -> bool {
        if index >= self.account_keys.len() {
            return false;
        }
        if self.is_signer(index) {
            // Signed accounts: writable unless in the readonly_signed range
            let readonly_signed_start =
                self.num_required_signatures as usize - self.num_readonly_signed as usize;
            index < readonly_signed_start
        } else {
            // Unsigned accounts: writable unless in the readonly_unsigned range
            let unsigned_start = self.num_required_signatures as usize;
            let unsigned_writable_count = self.account_keys.len()
                - self.num_required_signatures as usize
                - self.num_readonly_unsigned as usize;
            index < unsigned_start + unsigned_writable_count
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::{AccountMeta, Instruction};
    use nusantara_crypto::hash;

    #[test]
    fn new_message_with_single_instruction() {
        let payer = hash(b"payer");
        let program = hash(b"program");
        let account = hash(b"account");

        let ix = Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(account, false)],
            data: vec![1, 2, 3],
        };

        let msg = Message::new(&[ix], &payer).unwrap();
        assert_eq!(msg.num_required_signatures, 1); // payer
        assert_eq!(msg.account_keys.len(), 3); // payer, account (writable), program (readonly)
        assert_eq!(msg.account_keys[0], payer);
        // account is unsigned-writable, program is unsigned-readonly
        // sorted: [payer, account, program]
        assert_eq!(msg.account_keys[1], account);
        assert_eq!(msg.account_keys[2], program);
        assert_eq!(msg.instructions.len(), 1);
    }

    #[test]
    fn empty_instructions_error() {
        let payer = hash(b"payer");
        assert!(Message::new(&[], &payer).is_err());
    }

    #[test]
    fn borsh_roundtrip() {
        let msg = Message {
            num_required_signatures: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 1,
            account_keys: vec![hash(b"payer"), hash(b"program")],
            recent_blockhash: hash(b"blockhash"),
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: vec![42],
            }],
        };
        let encoded = borsh::to_vec(&msg).unwrap();
        let decoded: Message = borsh::from_slice(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }
}
