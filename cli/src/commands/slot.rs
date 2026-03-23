use nusantara_rpc::types::SlotResponse;

use crate::client::RpcClient;
use crate::error::CliError;
use crate::output;

pub async fn run(url: &str, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: SlotResponse = client.get("/v1/slot").await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Current slot:        {}", resp.slot);
        if let Some(stored) = resp.latest_stored_slot {
            println!("Latest stored slot:  {stored}");
        }
        if let Some(root) = resp.latest_root {
            println!("Latest root:         {root}");
        }
    }
    Ok(())
}
