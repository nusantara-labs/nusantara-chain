use reqwest::Client;
use serde::de::DeserializeOwned;

use crate::error::CliError;

pub struct RpcClient {
    client: Client,
    base_url: String,
}

impl RpcClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        let url = format!("{}{path}", self.base_url);
        let resp = self.client.get(&url).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Rpc(format!("{status}: {body}")));
        }

        resp.json().await.map_err(CliError::from)
    }

    pub async fn post<T: DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, CliError> {
        let url = format!("{}{path}", self.base_url);
        let resp = self.client.post(&url).json(body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CliError::Rpc(format!("{status}: {body}")));
        }

        resp.json().await.map_err(CliError::from)
    }
}
