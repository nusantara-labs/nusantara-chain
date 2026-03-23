use nusantara_rpc::types::AccountResponse;

use crate::client::RpcClient;
use crate::error::CliError;
use crate::keypair;
use crate::output;

pub async fn run(
    url: &str,
    keypair_path: &str,
    address: Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let addr = match address {
        Some(a) => a,
        None => {
            let kp = keypair::load_keypair(keypair_path)?;
            kp.address().to_base64()
        }
    };

    let client = RpcClient::new(url);
    let resp: AccountResponse = client
        .get(&format!("/v1/account/{addr}"))
        .await
        .map_err(|e| {
            if e.to_string().contains("404") {
                CliError::Rpc(format!("account not found: {addr}"))
            } else {
                e
            }
        })?;

    if json {
        output::print_json(&resp, true)?;
    } else {
        println!("{} NUSA ({} lamports)", resp.nusa, resp.lamports);
    }
    Ok(())
}
