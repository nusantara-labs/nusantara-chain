use nusantara_rpc::types::BlockResponse;

use crate::client::RpcClient;
use crate::error::CliError;
use crate::output;

pub async fn run(url: &str, slot: u64, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: BlockResponse = client.get(&format!("/v1/block/{slot}")).await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Slot:        {}", resp.slot);
        println!("Parent slot: {}", resp.parent_slot);
        println!("Block hash:  {}", resp.block_hash);
        println!("Parent hash: {}", resp.parent_hash);
        println!("Timestamp:   {}", resp.timestamp);
        println!("Validator:   {}", resp.validator);
        println!("Tx count:    {}", resp.transaction_count);
        println!("Merkle root: {}", resp.merkle_root);
    }
    Ok(())
}
