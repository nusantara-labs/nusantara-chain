use nusantara_rpc::types::TransactionStatusResponse;

use crate::client::RpcClient;
use crate::error::CliError;
use crate::output;

pub async fn run(url: &str, hash: &str, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: TransactionStatusResponse =
        client.get(&format!("/v1/transaction/{hash}")).await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Signature: {}", resp.signature);
        println!("Slot:      {}", resp.slot);
        println!("Status:    {}", resp.status);
        println!("Fee:       {} lamports", resp.fee);
        println!("CU used:   {}", resp.compute_units_consumed);
    }
    Ok(())
}
