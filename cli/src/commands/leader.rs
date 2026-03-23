use nusantara_rpc::types::LeaderScheduleResponse;

use crate::client::RpcClient;
use crate::error::CliError;
use crate::output;

pub async fn run(url: &str, epoch: Option<u64>, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);

    let resp: LeaderScheduleResponse = match epoch {
        Some(e) => client.get(&format!("/v1/leader-schedule/{e}")).await?,
        None => client.get("/v1/leader-schedule").await?,
    };

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Leader Schedule — Epoch {}", resp.epoch);
        println!("{:-<60}", "");

        // Group consecutive slots by leader
        let mut i = 0;
        while i < resp.schedule.len() {
            let leader = &resp.schedule[i].leader;
            let start_slot = resp.schedule[i].slot;
            let mut end_slot = start_slot;

            while i + 1 < resp.schedule.len() && resp.schedule[i + 1].leader == *leader {
                i += 1;
                end_slot = resp.schedule[i].slot;
            }

            if start_slot == end_slot {
                println!("  Slot {start_slot}: {leader}");
            } else {
                println!("  Slots {start_slot}-{end_slot}: {leader}");
            }
            i += 1;
        }
    }
    Ok(())
}
