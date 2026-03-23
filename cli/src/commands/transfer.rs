use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nusantara_core::{Message, Transaction, nusa_to_lamports};
use nusantara_crypto::Hash;
use nusantara_rpc::types::{BlockhashResponse, SendTransactionRequest, SendTransactionResponse};

use crate::client::RpcClient;
use crate::error::CliError;
use crate::keypair;

pub async fn run(
    url: &str,
    keypair_path: &str,
    to: &str,
    amount: f64,
    json: bool,
) -> Result<(), CliError> {
    let kp = keypair::load_keypair(keypair_path)?;
    let from = kp.address();
    let to_hash = Hash::from_base64(to)
        .map_err(|e| CliError::Parse(format!("invalid recipient address: {e}")))?;

    let lamports = nusa_to_lamports(amount);
    let client = RpcClient::new(url);

    // Get recent blockhash
    let bh_resp: BlockhashResponse = client.get("/v1/blockhash").await?;
    let blockhash = Hash::from_base64(&bh_resp.blockhash)
        .map_err(|e| CliError::Parse(format!("invalid blockhash: {e}")))?;

    // Build transfer instruction
    let ix = nusantara_system_program::transfer(&from, &to_hash, lamports);
    let mut msg = Message::new(&[ix], &from)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[&kp]);

    let bytes = borsh::to_vec(&tx)
        .map_err(|e| CliError::Serialization(e.to_string()))?;
    let encoded = URL_SAFE_NO_PAD.encode(&bytes);

    let resp: SendTransactionResponse = client
        .post(
            "/v1/transaction/send",
            &SendTransactionRequest {
                transaction: encoded,
            },
        )
        .await?;

    if json {
        println!(
            "{}",
            serde_json::json!({ "signature": resp.signature })
        );
    } else {
        println!("Transfer sent: {amount} NUSA to {to}");
        println!("Signature: {}", resp.signature);
    }
    Ok(())
}
