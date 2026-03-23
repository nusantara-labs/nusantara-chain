use nusantara_rpc::types::EpochInfoResponse;

use crate::client::RpcClient;
use crate::error::CliError;
use crate::output;

pub async fn run(url: &str, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: EpochInfoResponse = client.get("/v1/epoch-info").await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Epoch:                {}", resp.epoch);
        println!("Slot index:           {}/{}", resp.slot_index, resp.slots_in_epoch);
        println!("Absolute slot:        {}", resp.absolute_slot);
        println!("Timestamp:            {}", resp.timestamp);
        println!("Leader schedule epoch: {}", resp.leader_schedule_epoch);

        let progress = resp.slot_index as f64 / resp.slots_in_epoch as f64 * 100.0;
        println!("Epoch progress:       {progress:.1}%");
    }
    Ok(())
}
