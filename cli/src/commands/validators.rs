use nusantara_core::lamports_to_nusa;
use nusantara_rpc::types::ValidatorsResponse;

use crate::client::RpcClient;
use crate::error::CliError;
use crate::output;

pub async fn run(url: &str, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: ValidatorsResponse = client.get("/v1/validators").await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!(
            "Total active stake: {} NUSA",
            lamports_to_nusa(resp.total_active_stake)
        );
        println!("{:-<80}", "");
        println!(
            "{:<20} {:<10} {:>15} {:>10} {:>10}",
            "Identity", "Comm%", "Stake (NUSA)", "Last Vote", "Root"
        );
        println!("{:-<80}", "");

        for v in &resp.validators {
            let short_id = if v.identity.len() > 16 {
                format!("{}...", &v.identity[..16])
            } else {
                v.identity.clone()
            };
            let stake_nusa = lamports_to_nusa(v.active_stake);
            let last_vote = v
                .last_vote
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".to_string());
            let root = v
                .root_slot
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".to_string());

            println!(
                "{:<20} {:<10} {:>15.2} {:>10} {:>10}",
                short_id, v.commission, stake_nusa, last_vote, root
            );
        }
    }
    Ok(())
}
