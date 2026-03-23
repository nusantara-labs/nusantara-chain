use nusantara_core::message::Message;
use nusantara_core::transaction::Transaction;
use nusantara_crypto::{Hash, Keypair};
use nusantara_vote_program::{Vote, vote};

/// Build a signed vote transaction.
///
/// The validator submits this each slot to record its vote in the Tower BFT.
pub fn build_vote_transaction(
    keypair: &Keypair,
    vote_account: &Hash,
    v: Vote,
    recent_blockhash: Hash,
) -> Transaction {
    let identity_address = keypair.address();
    let instruction = vote(vote_account, &identity_address, v);
    let mut message =
        Message::new(&[instruction], &identity_address).expect("vote message construction");
    message.recent_blockhash = recent_blockhash;
    let mut tx = Transaction::new(message);
    tx.sign(&[keypair]);
    tx
}
