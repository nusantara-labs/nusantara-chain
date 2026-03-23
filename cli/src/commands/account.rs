use nusantara_rpc::types::AccountResponse;

use crate::client::RpcClient;
use crate::error::CliError;
use crate::output;

pub async fn run(url: &str, address: &str, json: bool) -> Result<(), CliError> {
    let client = RpcClient::new(url);
    let resp: AccountResponse = client.get(&format!("/v1/account/{address}")).await?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("Address:    {}", resp.address);
        println!("Balance:    {} NUSA ({} lamports)", resp.nusa, resp.lamports);
        println!("Owner:      {}", resp.owner);
        println!("Executable: {}", resp.executable);
        println!("Data size:  {} bytes", resp.data_len);
        println!("Rent epoch: {}", resp.rent_epoch);
    }
    Ok(())
}
