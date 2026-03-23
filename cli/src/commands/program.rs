use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nusantara_core::{Message, Transaction};
use nusantara_crypto::{Hash, Keypair, hashv};
use nusantara_rpc::types::{
    BlockhashResponse, ProgramResponse, SendTransactionRequest, SendTransactionResponse,
};

use crate::client::RpcClient;
use crate::error::CliError;
use crate::keypair;
use crate::output;

const WRITE_CHUNK_SIZE: usize = 1024;

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

/// Write WASM bytecode to a buffer account in chunks.
async fn write_buffer(
    client: &RpcClient,
    buffer: &Hash,
    payer: &Keypair,
    wasm_bytes: &[u8],
) -> Result<(), CliError> {
    let payer_addr = payer.address();

    for (i, chunk) in wasm_bytes.chunks(WRITE_CHUNK_SIZE).enumerate() {
        let offset = (i * WRITE_CHUNK_SIZE) as u32;
        let ix = nusantara_loader_program::write(buffer, &payer_addr, offset, chunk.to_vec());

        let blockhash = get_blockhash(client).await?;
        let mut msg = Message::new(&[ix], &payer_addr)
            .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
        msg.recent_blockhash = blockhash;

        let mut tx = Transaction::new(msg);
        tx.sign(&[payer]);
        send_tx(client, &tx).await?;
    }
    Ok(())
}

pub async fn deploy(
    url: &str,
    keypair_path: &str,
    wasm_path: &str,
    json: bool,
) -> Result<(), CliError> {
    let payer = keypair::load_keypair(keypair_path)?;
    let payer_addr = payer.address();

    let wasm_bytes = std::fs::read(wasm_path)
        .map_err(CliError::Io)?;

    let buffer_kp = Keypair::generate();
    let buffer_addr = buffer_kp.address();
    let program_kp = Keypair::generate();
    let program_addr = program_kp.address();
    let program_data_addr = hashv(&[b"program_data", program_addr.as_bytes()]);

    let client = RpcClient::new(url);

    // Tx 1: InitializeBuffer
    let init_ix = nusantara_loader_program::initialize_buffer(&buffer_addr, &payer_addr);
    let blockhash = get_blockhash(&client).await?;
    let mut msg = Message::new(&[init_ix], &payer_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer, &buffer_kp]);
    send_tx(&client, &tx).await?;

    // Tx 2..N: Write chunks
    write_buffer(&client, &buffer_addr, &payer, &wasm_bytes).await?;

    // Tx final: Deploy
    let max_data_len = wasm_bytes.len() as u64 * 2; // allow room for upgrades
    let deploy_ix = nusantara_loader_program::deploy(
        &payer_addr,
        &program_addr,
        &program_data_addr,
        &buffer_addr,
        &payer_addr,
        max_data_len,
    );
    let blockhash = get_blockhash(&client).await?;
    let mut msg = Message::new(&[deploy_ix], &payer_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer, &program_kp]);
    let sig = send_tx(&client, &tx).await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "signature": sig,
                "program_address": program_addr.to_base64(),
                "program_data_address": program_data_addr.to_base64(),
            })
        );
    } else {
        println!("Program deployed: {}", program_addr.to_base64());
        println!("Program data:     {}", program_data_addr.to_base64());
        println!("Signature: {sig}");
    }
    Ok(())
}

pub async fn show(url: &str, address: &str, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: ProgramResponse = client.get(&format!("/v1/program/{address}")).await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Program:       {}", resp.address);
        println!("Executable:    {}", resp.executable);
        println!("Data address:  {}", resp.program_data_address);
        println!("Authority:     {}", resp.authority.as_deref().unwrap_or("none (immutable)"));
        println!("Deploy slot:   {}", resp.deploy_slot);
        println!("Bytecode size: {} bytes", resp.bytecode_size);
        println!("Balance:       {} lamports", resp.lamports);
    }
    Ok(())
}

pub async fn upgrade(
    url: &str,
    keypair_path: &str,
    program_address: &str,
    wasm_path: &str,
    json: bool,
) -> Result<(), CliError> {
    let payer = keypair::load_keypair(keypair_path)?;
    let payer_addr = payer.address();

    let wasm_bytes = std::fs::read(wasm_path)
        .map_err(CliError::Io)?;

    let client = RpcClient::new(url);

    // Get existing program info
    let program_info: ProgramResponse = client
        .get(&format!("/v1/program/{program_address}"))
        .await?;

    let program_hash = Hash::from_base64(program_address)
        .map_err(|e| CliError::Parse(format!("invalid program address: {e}")))?;
    let program_data_hash = Hash::from_base64(&program_info.program_data_address)
        .map_err(|e| CliError::Parse(format!("invalid program data address: {e}")))?;

    // Create and write new buffer
    let buffer_kp = Keypair::generate();
    let buffer_addr = buffer_kp.address();

    let init_ix = nusantara_loader_program::initialize_buffer(&buffer_addr, &payer_addr);
    let blockhash = get_blockhash(&client).await?;
    let mut msg = Message::new(&[init_ix], &payer_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer, &buffer_kp]);
    send_tx(&client, &tx).await?;

    write_buffer(&client, &buffer_addr, &payer, &wasm_bytes).await?;

    // Upgrade
    let upgrade_ix = nusantara_loader_program::upgrade(
        &program_hash,
        &program_data_hash,
        &buffer_addr,
        &payer_addr,
    );
    let blockhash = get_blockhash(&client).await?;
    let mut msg = Message::new(&[upgrade_ix], &payer_addr)
        .map_err(|e| CliError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = blockhash;
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer]);
    let sig = send_tx(&client, &tx).await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "signature": sig,
                "program_address": program_address,
            })
        );
    } else {
        println!("Program upgraded: {program_address}");
        println!("Signature: {sig}");
    }
    Ok(())
}
