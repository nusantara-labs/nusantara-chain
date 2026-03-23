use std::time::Duration;

use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tracing::warn;

use crate::error::E2eError;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub timeout: Duration,
    pub max_retries: u32,
    pub retry_backoff: Duration,
    pub pool_max_idle_per_host: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            max_retries: 3,
            retry_backoff: Duration::from_millis(500),
            pool_max_idle_per_host: 4,
        }
    }
}

pub struct NusantaraClient {
    urls: Vec<String>,
    client: Client,
    config: ClientConfig,
}

impl NusantaraClient {
    pub fn new(urls: Vec<String>, config: ClientConfig) -> Self {
        let client = Client::builder()
            .timeout(config.timeout)
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .build()
            .expect("failed to build HTTP client");

        let urls = urls
            .into_iter()
            .map(|u| u.trim_end_matches('/').to_string())
            .collect();

        Self {
            urls,
            client,
            config,
        }
    }

    pub fn primary_url(&self) -> &str {
        &self.urls[0]
    }

    pub fn node_count(&self) -> usize {
        self.urls.len()
    }

    /// GET from the primary node.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, E2eError> {
        self.get_from(0, path).await
    }

    /// GET from a specific node by index.
    pub async fn get_from<T: DeserializeOwned>(
        &self,
        node: usize,
        path: &str,
    ) -> Result<T, E2eError> {
        let base = &self.urls[node];
        let url = format!("{base}{path}");
        self.get_url(&url).await
    }

    /// GET from all nodes in parallel, returning results in node order.
    pub async fn get_all<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
    ) -> Vec<Result<T, E2eError>> {
        let mut handles = Vec::with_capacity(self.urls.len());
        for base in &self.urls {
            let url = format!("{base}{path}");
            let client = self.client.clone();
            let config = self.config.clone();
            handles.push(tokio::spawn(async move {
                retry_get::<T>(&client, &url, &config).await
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(
                handle
                    .await
                    .unwrap_or_else(|e| Err(E2eError::Other(format!("join error: {e}")))),
            );
        }
        results
    }

    /// POST JSON to the primary node.
    pub async fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, E2eError> {
        let url = format!("{}{path}", self.urls[0]);
        self.post_url(&url, body).await
    }

    /// POST JSON to a specific node by index.
    pub async fn post_to<T: DeserializeOwned, B: Serialize>(
        &self,
        node: usize,
        path: &str,
        body: &B,
    ) -> Result<T, E2eError> {
        let url = format!("{}{path}", self.urls[node]);
        self.post_url(&url, body).await
    }

    async fn get_url<T: DeserializeOwned>(&self, url: &str) -> Result<T, E2eError> {
        retry_get(&self.client, url, &self.config).await
    }

    async fn post_url<T: DeserializeOwned, B: Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, E2eError> {
        retry_post(&self.client, url, body, &self.config).await
    }
}

async fn retry_get<T: DeserializeOwned>(
    client: &Client,
    url: &str,
    config: &ClientConfig,
) -> Result<T, E2eError> {
    let mut last_err = None;
    for attempt in 0..=config.max_retries {
        if attempt > 0 {
            let backoff = config.retry_backoff * 2u32.saturating_pow(attempt - 1);
            tokio::time::sleep(backoff).await;
        }

        match client.get(url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    return resp.json().await.map_err(E2eError::Http);
                }
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                // Don't retry 4xx client errors
                if status < 500 {
                    return Err(E2eError::Rpc { status, body });
                }
                warn!(attempt, status, %body, "retryable server error on GET {url}");
                last_err = Some(E2eError::Rpc { status, body });
            }
            Err(e) => {
                warn!(attempt, %e, "HTTP GET error on {url}");
                last_err = Some(E2eError::Http(e));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| E2eError::Other("no attempts made".into())))
}

async fn retry_post<T: DeserializeOwned, B: Serialize>(
    client: &Client,
    url: &str,
    body: &B,
    config: &ClientConfig,
) -> Result<T, E2eError> {
    let mut last_err = None;
    for attempt in 0..=config.max_retries {
        if attempt > 0 {
            let backoff = config.retry_backoff * 2u32.saturating_pow(attempt - 1);
            tokio::time::sleep(backoff).await;
        }

        match client.post(url).json(body).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    return resp.json().await.map_err(E2eError::Http);
                }
                let status = resp.status().as_u16();
                let body_text = resp.text().await.unwrap_or_default();
                if status < 500 {
                    return Err(E2eError::Rpc {
                        status,
                        body: body_text,
                    });
                }
                warn!(attempt, status, %body_text, "retryable server error on POST {url}");
                last_err = Some(E2eError::Rpc {
                    status,
                    body: body_text,
                });
            }
            Err(e) => {
                warn!(attempt, %e, "HTTP POST error on {url}");
                last_err = Some(E2eError::Http(e));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| E2eError::Other("no attempts made".into())))
}
