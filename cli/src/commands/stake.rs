use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nusantara_core::{Message, Transaction, nusa_to_lamports};
use nusantara_crypto::Hash;
use nusantara_rpc::types::{
    BlockhashResponse, SendTransactionRequest, SendTransactionResponse, StakeAccountResponse,
};
use nusantara_stake_program::{Authorized, Lockup};

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
    stake_keypair_path: &str,
    amount: f64,
    json: bool,
) -> Result<(), CliError> {
    let payer = keypair::load_keypair(keypair_path)?;
    let stake_kp = keypair::load_keypair(stake_keypair_path)?;
    let payer_addr = payer.address();
    let stake_addr = stake_kp.address();
    let lamports = nusa_to_lamports(amount);

    let client = RpcClient::new(url);
    let blockhash = get_blockhash(&client).await?;

    // Calculate minimum stake account size (StakeStateV2::Initialized)
    let space = 200; // sufficient for borsh-serialized StakeStateV2
    let create_ix = nusantara_system_program::create_account(
        &payer_addr,
        &stake_addr,
        lamports,
        space,
        &nusantara_core::STAKE_PROGRAM_ID,
    );

    let init_ix = nusantara_stake_program::initialize(
        &stake_addr,
        Authorized {
            staker: payer_addr,
            withdrawer: payer_addr,
        },
        Lockup {
            unix_timestamp: 0,
            epoch: 0,
            custodian: Hash::zero(),
        },
    );

    let mut msg = Message::new(&[create_ix, init_ix], &payer_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer, &stake_kp]);

    let sig = send_tx(&client, &tx).await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "signature": sig,
                "stake_account": stake_addr.to_base64(),
            })
        );
    } else {
        println!("Stake account created: {}", stake_addr.to_base64());
        println!("Signature: {sig}");
    }
    Ok(())
}

pub async fn delegate(
    url: &str,
    keypair_path: &str,
    stake_account: &str,
    vote_account: &str,
    json: bool,
) -> Result<(), CliError> {
    let staker = keypair::load_keypair(keypair_path)?;
    let staker_addr = staker.address();
    let stake_hash = Hash::from_base64(stake_account)
        .map_err(|e| CliError::Parse(format!("invalid stake address: {e}")))?;
    let vote_hash = Hash::from_base64(vote_account)
        .map_err(|e| CliError::Parse(format!("invalid vote address: {e}")))?;

    let client = RpcClient::new(url);
    let blockhash = get_blockhash(&client).await?;

    let ix = nusantara_stake_program::delegate_stake(&stake_hash, &vote_hash, &staker_addr);
    let mut msg = Message::new(&[ix], &staker_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[&staker]);

    let sig = send_tx(&client, &tx).await?;
    if json {
        println!("{}", serde_json::json!({ "signature": sig }));
    } else {
        println!("Stake delegated to {vote_account}");
        println!("Signature: {sig}");
    }
    Ok(())
}

pub async fn deactivate(
    url: &str,
    keypair_path: &str,
    stake_account: &str,
    json: bool,
) -> Result<(), CliError> {
    let staker = keypair::load_keypair(keypair_path)?;
    let staker_addr = staker.address();
    let stake_hash = Hash::from_base64(stake_account)
        .map_err(|e| CliError::Parse(format!("invalid stake address: {e}")))?;

    let client = RpcClient::new(url);
    let blockhash = get_blockhash(&client).await?;

    let ix = nusantara_stake_program::deactivate(&stake_hash, &staker_addr);
    let mut msg = Message::new(&[ix], &staker_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[&staker]);

    let sig = send_tx(&client, &tx).await?;
    if json {
        println!("{}", serde_json::json!({ "signature": sig }));
    } else {
        println!("Stake deactivated: {stake_account}");
        println!("Signature: {sig}");
    }
    Ok(())
}

pub async fn withdraw(
    url: &str,
    keypair_path: &str,
    stake_account: &str,
    to: &str,
    amount: f64,
    json: bool,
) -> Result<(), CliError> {
    let withdrawer = keypair::load_keypair(keypair_path)?;
    let withdrawer_addr = withdrawer.address();
    let stake_hash = Hash::from_base64(stake_account)
        .map_err(|e| CliError::Parse(format!("invalid stake address: {e}")))?;
    let to_hash = Hash::from_base64(to)
        .map_err(|e| CliError::Parse(format!("invalid recipient address: {e}")))?;
    let lamports = nusa_to_lamports(amount);

    let client = RpcClient::new(url);
    let blockhash = get_blockhash(&client).await?;

    let ix = nusantara_stake_program::withdraw(&stake_hash, &withdrawer_addr, &to_hash, lamports);
    let mut msg = Message::new(&[ix], &withdrawer_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[&withdrawer]);

    let sig = send_tx(&client, &tx).await?;
    if json {
        println!("{}", serde_json::json!({ "signature": sig }));
    } else {
        println!("Withdrew {amount} NUSA from {stake_account} to {to}");
        println!("Signature: {sig}");
    }
    Ok(())
}

pub async fn view(url: &str, address: &str, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: StakeAccountResponse =
        client.get(&format!("/v1/stake-account/{address}")).await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Stake Account: {}", resp.address);
        println!("Balance:       {} lamports", resp.lamports);
        println!("State:         {}", resp.state);
        if let Some(staker) = &resp.staker {
            println!("Staker:        {staker}");
        }
        if let Some(withdrawer) = &resp.withdrawer {
            println!("Withdrawer:    {withdrawer}");
        }
        if let Some(voter) = &resp.voter {
            println!("Voter:         {voter}");
        }
        if let Some(stake) = resp.stake {
            println!("Stake:         {stake} lamports");
        }
        if let Some(epoch) = resp.activation_epoch {
            println!("Activation:    epoch {epoch}");
        }
        if let Some(epoch) = resp.deactivation_epoch {
            println!("Deactivation:  epoch {epoch}");
        }
    }
    Ok(())
}
