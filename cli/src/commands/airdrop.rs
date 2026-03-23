use nusantara_core::nusa_to_lamports;
use nusantara_rpc::types::{AirdropRequest, AirdropResponse};

use crate::client::RpcClient;
use crate::error::CliError;
use crate::keypair;

pub async fn run(
    url: &str,
    keypair_path: &str,
    amount: f64,
    recipient: Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let address = match recipient {
        Some(a) => a,
        None => {
            let kp = keypair::load_keypair(keypair_path)?;
            kp.address().to_base64()
        }
    };

    let lamports = nusa_to_lamports(amount);
    let client = RpcClient::new(url);

    let resp: AirdropResponse = client
        .post(
            "/v1/airdrop",
            &AirdropRequest {
                address: address.clone(),
                lamports,
            },
        )
        .await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "signature": resp.signature,
                "address": address,
                "lamports": lamports,
            })
        );
    } else {
        println!("Airdrop: {amount} NUSA to {address}");
        println!("Signature: {}", resp.signature);
    }
    Ok(())
}
