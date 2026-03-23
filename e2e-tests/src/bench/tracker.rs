use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::client::NusantaraClient;
use crate::error::E2eError;
use crate::types::TransactionStatusResponse;

use super::sender::Submission;

/// A confirmed transaction record.
#[derive(Debug, Clone)]
pub struct Confirmation {
    pub signature: String,
    pub submit_time: Instant,
    pub confirm_time: Instant,
    pub latency: Duration,
    pub status: String,
}

/// Result of tracking: confirmed, failed, or timed-out transactions.
#[derive(Debug)]
pub struct TrackingResult {
    pub confirmed: Vec<Confirmation>,
    pub failed: Vec<Confirmation>,
    pub timed_out: Vec<String>,
}

/// A WS handle that has pre-subscribed to signatures before transactions are sent.
/// This eliminates the race condition where notifications fire before subscriptions.
pub struct PreSubscribedTracker {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    /// Events received during the subscription phase (before all ACKs arrived).
    early_events: Vec<String>,
}

impl PreSubscribedTracker {
    /// Connect to the WebSocket, subscribe to all signatures, and wait for
    /// all subscription ACKs. This guarantees that the server has registered
    /// every signature before the caller sends any transactions.
    pub async fn connect_and_subscribe(
        ws_url: &str,
        signatures: &[String],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await?;

        // Send all subscribe messages in one burst
        for sig in signatures {
            let msg = serde_json::json!({
                "method": "signatureSubscribe",
                "params": { "signature": sig }
            });
            ws.send(Message::Text(msg.to_string().into())).await?;
        }

        // Wait for all subscription ACKs from the server.
        // The server sends {"result": "subscribed", ...} AFTER inserting into
        // the subs.signatures HashSet, so once we receive all ACKs, we know
        // all subscriptions are registered server-side.
        let mut ack_count = 0usize;
        let mut early_events = Vec::new();
        let ack_deadline = Instant::now() + Duration::from_secs(30);

        while ack_count < signatures.len() {
            let remaining = ack_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                warn!(
                    received = ack_count,
                    expected = signatures.len(),
                    "timed out waiting for subscription ACKs"
                );
                break;
            }

            match tokio::time::timeout(remaining, ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    // Check if this is an ACK or an early event
                    if text.contains("\"result\"") && text.contains("\"subscribed\"") {
                        ack_count += 1;
                    } else {
                        // Could be a SignatureNotification from an early block
                        early_events.push(text.to_string());
                    }
                }
                Ok(Some(Ok(_))) => {} // Ping/Pong/Binary — ignore
                Ok(Some(Err(e))) => return Err(Box::new(e)),
                Ok(None) => return Err("WebSocket closed during subscription".into()),
                Err(_) => {
                    warn!(
                        received = ack_count,
                        expected = signatures.len(),
                        "timed out waiting for subscription ACKs"
                    );
                    break;
                }
            }
        }

        info!(
            subscriptions = ack_count,
            early_events = early_events.len(),
            "all signatures subscribed via WebSocket"
        );

        Ok(Self {
            ws,
            early_events,
        })
    }

    /// Wait for confirmations after transactions have been sent.
    /// Maps signatures to their submit_times from the submissions list.
    pub async fn collect(
        mut self,
        submissions: &[Submission],
        timeout: Duration,
    ) -> TrackingResult {
        // Only track signatures that were actually submitted (mempool may reject some)
        let mut pending: HashMap<String, Instant> = submissions
            .iter()
            .map(|s| (s.signature.clone(), s.submit_time))
            .collect();

        let mut confirmed = Vec::new();
        let mut failed = Vec::new();

        // Process events that arrived during the subscription phase first
        for text in &self.early_events {
            process_ws_event(text, &mut pending, &mut confirmed, &mut failed);
        }
        if !self.early_events.is_empty() {
            debug!(
                early_confirmed = confirmed.len(),
                early_failed = failed.len(),
                "processed early events from subscription phase"
            );
        }

        let deadline = Instant::now() + timeout;
        let mut ws_msg_count = 0u64;

        loop {
            if pending.is_empty() {
                break;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            let msg = tokio::time::timeout(remaining, self.ws.next()).await;

            match msg {
                Ok(Some(Ok(Message::Text(text)))) => {
                    ws_msg_count += 1;
                    if ws_msg_count <= 3 {
                        debug!(ws_msg_count, text_len = text.len(), text_preview = %&text[..text.len().min(200)], "WS message received");
                    }
                    let before = pending.len();
                    process_ws_event(&text, &mut pending, &mut confirmed, &mut failed);
                    let after = pending.len();
                    if before == after {
                        // Event didn't match any pending signature — log first few
                        if confirmed.len() + failed.len() < 3 {
                            debug!(text = %text, "WS event did not match any pending signature");
                        }
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) => {
                    info!("WebSocket closed by server");
                    break;
                }
                Ok(Some(Ok(msg_frame))) => {
                    debug!(?msg_frame, "WS non-text frame");
                }
                Ok(Some(Err(e))) => {
                    warn!(%e, "WebSocket read error");
                    break;
                }
                Ok(None) => {
                    info!("WebSocket stream ended");
                    break;
                }
                Err(_) => {
                    info!(
                        pending = pending.len(),
                        confirmed = confirmed.len(),
                        failed = failed.len(),
                        ws_msg_count,
                        "confirmation tracking timed out"
                    );
                    break;
                }
            }
        }

        let timed_out: Vec<String> = pending.into_keys().collect();

        if !failed.is_empty() {
            // Log first few failure reasons for diagnosis
            for c in failed.iter().take(3) {
                warn!(
                    sig = %c.signature,
                    status = %c.status,
                    latency_ms = c.latency.as_millis(),
                    "transaction failed"
                );
            }
            if failed.len() > 3 {
                warn!(
                    total_failed = failed.len(),
                    "... and more failures (showing first 3)"
                );
            }
        }

        TrackingResult {
            confirmed,
            failed,
            timed_out,
        }
    }
}

/// Parse a WS text message and update tracking state.
fn process_ws_event(
    text: &str,
    pending: &mut HashMap<String, Instant>,
    confirmed: &mut Vec<Confirmation>,
    failed: &mut Vec<Confirmation>,
) {
    if let Ok(event) = serde_json::from_str::<WsEvent>(text)
        && event.r#type == "SignatureNotification"
        && let (Some(sig), Some(status)) = (event.signature, event.status)
    {
        let confirm_time = Instant::now();
        if let Some(submit_time) = pending.remove(&sig) {
            let latency = confirm_time.duration_since(submit_time);
            let confirmation = Confirmation {
                signature: sig,
                submit_time,
                confirm_time,
                latency,
                status: status.clone(),
            };
            if status == "success" {
                confirmed.push(confirmation);
            } else {
                failed.push(confirmation);
            }
        }
    }
}

/// Track confirmation of submitted transactions.
///
/// Tries WebSocket signatureSubscribe first, falls back to HTTP polling.
pub async fn track(
    client: Arc<NusantaraClient>,
    submissions: &[Submission],
    timeout: Duration,
) -> TrackingResult {
    let rpc_url = client.primary_url();
    let ws_url = rpc_url
        .replace("http://", "ws://")
        .replace("https://", "wss://")
        + "/v1/ws";

    match track_ws(&ws_url, submissions, timeout).await {
        Ok(result) => result,
        Err(e) => {
            warn!(%e, "WebSocket tracking failed, falling back to HTTP polling");
            track_http(client, submissions, timeout).await
        }
    }
}

/// WebSocket-based tracking (post-hoc subscription — may miss early confirmations).
async fn track_ws(
    ws_url: &str,
    submissions: &[Submission],
    timeout: Duration,
) -> Result<TrackingResult, Box<dyn std::error::Error + Send + Sync>> {
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await?;

    let mut pending: HashMap<String, Instant> = submissions
        .iter()
        .map(|s| (s.signature.clone(), s.submit_time))
        .collect();

    for sig in pending.keys() {
        let msg = serde_json::json!({
            "method": "signatureSubscribe",
            "params": { "signature": sig }
        });
        ws.send(Message::Text(msg.to_string().into())).await?;
    }

    let mut confirmed = Vec::new();
    let mut failed = Vec::new();
    let deadline = Instant::now() + timeout;

    loop {
        if pending.is_empty() {
            break;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        let msg = tokio::time::timeout(remaining, ws.next()).await;

        match msg {
            Ok(Some(Ok(Message::Text(text)))) => {
                process_ws_event(&text, &mut pending, &mut confirmed, &mut failed);
            }
            Ok(Some(Ok(Message::Close(_)))) => break,
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(e))) => {
                warn!(%e, "WebSocket read error");
                break;
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let timed_out: Vec<String> = pending.into_keys().collect();

    Ok(TrackingResult {
        confirmed,
        failed,
        timed_out,
    })
}

/// Minimal struct to parse SignatureNotification events from the WS stream.
#[derive(Debug, serde::Deserialize)]
struct WsEvent {
    r#type: String,
    signature: Option<String>,
    status: Option<String>,
}

/// HTTP polling fallback.
async fn track_http(
    client: Arc<NusantaraClient>,
    submissions: &[Submission],
    timeout: Duration,
) -> TrackingResult {
    use tokio::task::JoinSet;

    let poll_interval = Duration::from_millis(500);
    let batch_size = 256;
    let start = Instant::now();

    let mut pending: Vec<Submission> = submissions.to_vec();
    let mut confirmed = Vec::new();
    let mut failed = Vec::new();

    while !pending.is_empty() && start.elapsed() < timeout {
        let mut still_pending = Vec::new();

        for chunk in pending.chunks(batch_size) {
            let mut join_set = JoinSet::new();

            for sub in chunk {
                let sig = sub.signature.clone();
                let submit_time = sub.submit_time;
                let client = client.clone();

                join_set.spawn(async move {
                    let path = format!("/v1/transaction/{sig}");
                    let result = client.get::<TransactionStatusResponse>(&path).await;
                    (sig, submit_time, result)
                });
            }

            while let Some(join_result) = join_set.join_next().await {
                match join_result {
                    Ok((sig, submit_time, Ok(status))) => {
                        if status.status == "received" {
                            still_pending.push(Submission {
                                signature: sig,
                                submit_time,
                            });
                            continue;
                        }
                        let confirm_time = Instant::now();
                        let latency = confirm_time.duration_since(submit_time);
                        if status.status != "success" {
                            warn!(
                                %sig,
                                status = %status.status,
                                slot = status.slot,
                                fee = status.fee,
                                "transaction failed"
                            );
                        }
                        let confirmation = Confirmation {
                            signature: sig,
                            submit_time,
                            confirm_time,
                            latency,
                            status: status.status.clone(),
                        };
                        if status.status == "success" {
                            confirmed.push(confirmation);
                        } else {
                            failed.push(confirmation);
                        }
                    }
                    Ok((sig, submit_time, Err(E2eError::Rpc { status: 404, .. }))) => {
                        still_pending.push(Submission {
                            signature: sig,
                            submit_time,
                        });
                    }
                    Ok((sig, submit_time, Err(e))) => {
                        warn!(%sig, %e, "error polling tx status");
                        still_pending.push(Submission {
                            signature: sig,
                            submit_time,
                        });
                    }
                    Err(e) => {
                        warn!(%e, "tracker poll task panicked");
                    }
                }
            }
        }

        pending = still_pending;

        if !pending.is_empty() {
            debug!(remaining = pending.len(), "waiting for confirmations");
            tokio::time::sleep(poll_interval).await;
        }
    }

    let timed_out: Vec<String> = pending.iter().map(|s| s.signature.clone()).collect();

    TrackingResult {
        confirmed,
        failed,
        timed_out,
    }
}
