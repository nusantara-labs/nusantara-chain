use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nusantara_core::{Message, Transaction};
use nusantara_crypto::Hash;
use nusantara_rpc::types::{
    BlockhashResponse, SendTransactionRequest, SendTransactionResponse, VoteAccountResponse,
};
use nusantara_vote_program::{VoteAuthorize, VoteInit};

use crate::client::RpcClient;
use crate::error::CliError;
use crate::keypair;
use crate::output;

async fn get_blockhash(client: &RpcClient) -> Result<Hash, CliError> {
    let resp: BlockhashResponse = client.get("/v1/blockhash").await?;
    Hash::from_base64(&resp.blockhash)
        .map_err(|e| CliError::Parse(format!("invalid blockhash: {e}")))
}

async fn send_tx(client: &RpcClient, tx: &Transaction) -> Result<String, CliError> {
    let bytes = borsh::to_vec(tx).map_err(|e| CliError::Serialization(e.to_string()))?;
    let encoded = URL_SAFE_NO_PAD.encode(&bytes);
    let resp: SendTransactionResponse = client
        .post(
            "/v1/transaction/send",
            &SendTransactionRequest {
                transaction: encoded,
            },
        )
        .await?;
    Ok(resp.signature)
}

pub async fn create(
    url: &str,
    keypair_path: &str,
    vote_keypair_path: &str,
    identity: &str,
    commission: u8,
    json: bool,
) -> Result<(), CliError> {
    let payer = keypair::load_keypair(keypair_path)?;
    let vote_kp = keypair::load_keypair(vote_keypair_path)?;
    let payer_addr = payer.address();
    let vote_addr = vote_kp.address();
    let identity_hash = Hash::from_base64(identity)
        .map_err(|e| CliError::Parse(format!("invalid identity: {e}")))?;

    let client = RpcClient::new(url);
    let blockhash = get_blockhash(&client).await?;

    let space = 3762; // VoteState borsh size
    let lamports = 10_000_000; // rent-exempt minimum for vote account

    let create_ix = nusantara_system_program::create_account(
        &payer_addr,
        &vote_addr,
        lamports,
        space,
        &nusantara_core::VOTE_PROGRAM_ID,
    );

    let init_ix = nusantara_vote_program::initialize_account(
        &vote_addr,
        VoteInit {
            node_pubkey: identity_hash,
            authorized_voter: payer_addr,
            authorized_withdrawer: payer_addr,
            commission,
        },
    );

    let mut msg = Message::new(&[create_ix, init_ix], &payer_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer, &vote_kp]);

    let sig = send_tx(&client, &tx).await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "signature": sig,
                "vote_account": vote_addr.to_base64(),
            })
        );
    } else {
        println!("Vote account created: {}", vote_addr.to_base64());
        println!("Signature: {sig}");
    }
    Ok(())
}

pub async fn view(url: &str, address: &str, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: VoteAccountResponse =
        client.get(&format!("/v1/vote-account/{address}")).await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Vote Account:     {}", resp.address);
        println!("Balance:          {} lamports", resp.lamports);
        println!("Node identity:    {}", resp.node_pubkey);
        println!("Voter:            {}", resp.authorized_voter);
        println!("Withdrawer:       {}", resp.authorized_withdrawer);
        println!("Commission:       {}%", resp.commission);
        if let Some(root) = resp.root_slot {
            println!("Root slot:        {root}");
        }
        if let Some(last) = resp.last_vote_slot {
            println!("Last vote:        slot {last}");
        }
        if !resp.epoch_credits.is_empty() {
            println!("Epoch credits:");
            for ec in &resp.epoch_credits {
                println!(
                    "  Epoch {}: {} credits (prev: {})",
                    ec.epoch, ec.credits, ec.prev_credits
                );
            }
        }
    }
    Ok(())
}

pub async fn authorize(
    url: &str,
    keypair_path: &str,
    vote_account: &str,
    new_auth: &str,
    auth_type: &str,
    json: bool,
) -> Result<(), CliError> {
    let authority = keypair::load_keypair(keypair_path)?;
    let authority_addr = authority.address();
    let vote_hash = Hash::from_base64(vote_account)
        .map_err(|e| CliError::Parse(format!("invalid vote address: {e}")))?;
    let new_auth_hash = Hash::from_base64(new_auth)
        .map_err(|e| CliError::Parse(format!("invalid new auth address: {e}")))?;

    let auth = match auth_type {
        "voter" => VoteAuthorize::Voter,
        "withdrawer" => VoteAuthorize::Withdrawer,
        _ => {
            return Err(CliError::Parse(
                "auth_type must be 'voter' or 'withdrawer'".to_string(),
            ))
        }
    };

    let client = RpcClient::new(url);
    let blockhash = get_blockhash(&client).await?;

    let ix = nusantara_vote_program::authorize(
        &vote_hash,
        &authority_addr,
        new_auth_hash,
        auth,
    );
    let mut msg = Message::new(&[ix], &authority_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[&authority]);

    let sig = send_tx(&client, &tx).await?;
    if json {
        println!("{}", serde_json::json!({ "signature": sig }));
    } else {
        println!("Vote {auth_type} authorized: {new_auth}");
        println!("Signature: {sig}");
    }
    Ok(())
}

pub async fn update_commission(
    url: &str,
    keypair_path: &str,
    vote_account: &str,
    commission: u8,
    json: bool,
) -> Result<(), CliError> {
    let authority = keypair::load_keypair(keypair_path)?;
    let authority_addr = authority.address();
    let vote_hash = Hash::from_base64(vote_account)
        .map_err(|e| CliError::Parse(format!("invalid vote address: {e}")))?;

    let client = RpcClient::new(url);
    let blockhash = get_blockhash(&client).await?;

    // Build the update commission instruction manually since the lib may not have it
    let data = borsh::to_vec(&nusantara_vote_program::VoteInstruction::UpdateCommission(
        commission,
    ))
    .map_err(|e| CliError::Serialization(e.to_string()))?;

    let ix = nusantara_core::Instruction {
        program_id: *nusantara_core::VOTE_PROGRAM_ID,
        accounts: vec![
            nusantara_core::AccountMeta::new(vote_hash, false),
            nusantara_core::AccountMeta::new_readonly(authority_addr, true),
        ],
        data,
    };

    let mut msg = Message::new(&[ix], &authority_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[&authority]);

    let sig = send_tx(&client, &tx).await?;
    if json {
        println!("{}", serde_json::json!({ "signature": sig }));
    } else {
        println!("Commission updated to {commission}%");
        println!("Signature: {sig}");
    }
    Ok(())
}
