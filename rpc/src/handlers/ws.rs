//! WebSocket subscription handler for real-time pubsub events.
//!
//! Clients connect to `/v1/ws` and send JSON subscription requests to receive
//! filtered event streams. The protocol is intentionally simple:
//!
//! **Subscribe**: `{"method": "slotSubscribe"}` or `{"method": "blockSubscribe"}`
//! **Unsubscribe**: `{"method": "slotUnsubscribe"}` or `{"method": "blockUnsubscribe"}`
//!
//! Events are delivered as JSON objects with a `"type"` discriminator field.
//!
//! Design notes:
//! - `WsConnGuard` (F20): RAII guard that decrements the active-connections
//!   gauge on drop, even if the handler panics.
//! - Send timeout (F9): every `socket.send` is wrapped in
//!   `tokio::time::timeout(WS_SEND_TIMEOUT)`. Timeout → warn + close.
//! - Consecutive lags (F9): two consecutive `Lagged` errors close the connection.
//! - Serialization (F19): events are serialized per-subscriber from the shared
//!   `PubsubEvent`. Moving to a pre-serialized `Arc<String>` broadcast would
//!   require changing `RpcState::pubsub_tx`'s channel type which is a
//!   validator-side coordination change; the per-subscriber serialization cost
//!   is acceptable for the current subscriber cardinality.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::{debug, instrument, warn};

use crate::server::{MAX_SUBSCRIPTIONS_PER_CONN, PubsubEvent, RpcState, WS_SEND_TIMEOUT_SECS};

/// WebSocket send timeout derived from build-time config.
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(WS_SEND_TIMEOUT_SECS);

// ---------------------------------------------------------------------------
// RAII gauge guard (F20)
// ---------------------------------------------------------------------------

/// Decrements the `nusantara_rpc_ws_active_connections` gauge on drop.
/// Constructed after the gauge is incremented; dropping it — even on panic —
/// keeps the metric accurate.
struct WsConnGuard;

impl Drop for WsConnGuard {
    fn drop(&mut self) {
        metrics::gauge!("nusantara_rpc_ws_active_connections").decrement(1.0);
    }
}

// ---------------------------------------------------------------------------
// Subscription state
// ---------------------------------------------------------------------------

/// Tracks which event types a client has subscribed to.
#[derive(Debug, Default)]
struct Subscriptions {
    slot: bool,
    block: bool,
    signatures: HashSet<String>,
}

impl Subscriptions {
    fn has_any(&self) -> bool {
        self.slot || self.block || !self.signatures.is_empty()
    }

    fn count(&self) -> usize {
        let mut n = self.signatures.len();
        if self.slot {
            n += 1;
        }
        if self.block {
            n += 1;
        }
        n
    }

    fn matches(&self, event: &PubsubEvent) -> bool {
        match event {
            PubsubEvent::SlotUpdate { .. } => self.slot,
            PubsubEvent::BlockNotification { .. } => self.block,
            PubsubEvent::SignatureNotification { signature, .. } => {
                self.signatures.contains(signature)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Client request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct SignatureParams {
    signature: String,
}

#[derive(Debug, Deserialize)]
struct ClientRequest {
    method: String,
    #[serde(default)]
    params: Option<SignatureParams>,
}

// ---------------------------------------------------------------------------
// Upgrade handler
// ---------------------------------------------------------------------------

/// Axum handler that upgrades an HTTP request to a WebSocket connection.
///
/// Acquires a permit from `RpcState::ws_semaphore` before upgrading.
/// If the limit is reached, returns 503 Service Unavailable.
#[instrument(skip_all)]
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<RpcState>>,
) -> impl IntoResponse {
    metrics::counter!("nusantara_rpc_ws_upgrades").increment(1);

    let permit = match state.ws_semaphore.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            warn!("WebSocket connection limit reached, rejecting upgrade");
            metrics::counter!("nusantara_rpc_ws_rejected_limit").increment(1);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "WebSocket connection limit reached",
            )
                .into_response();
        }
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, permit))
        .into_response()
}

// ---------------------------------------------------------------------------
// Session loop
// ---------------------------------------------------------------------------

/// Core WebSocket session loop.
///
/// Architecture:
/// - A `broadcast::Receiver<PubsubPayload>` is subscribed at connection start.
///   Events published before connection are not backfilled.
/// - `tokio::select!` concurrently polls client messages and broadcast events.
/// - Each send is wrapped in a `WS_SEND_TIMEOUT` timeout.
/// - Two consecutive `Lagged` errors close the connection.
/// - A `WsConnGuard` ensures the active-connections gauge is decremented on exit.
#[instrument(skip_all)]
async fn handle_socket(
    mut socket: WebSocket,
    state: Arc<RpcState>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    metrics::gauge!("nusantara_rpc_ws_active_connections").increment(1.0);
    let _guard = WsConnGuard; // decrements gauge on drop (F20)
    debug!("WebSocket client connected");

    let mut event_rx: broadcast::Receiver<PubsubEvent> = state.pubsub_tx.subscribe();
    let mut subs = Subscriptions::default();
    let mut consecutive_lags: u32 = 0;

    loop {
        tokio::select! {
            // Branch 1: Incoming message from the client.
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        handle_client_message(&text, &mut subs, &mut socket).await;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        debug!("WebSocket client disconnected");
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let send_result = tokio::time::timeout(
                            WS_SEND_TIMEOUT,
                            socket.send(Message::Pong(data)),
                        )
                        .await;
                        match send_result {
                            Ok(Ok(())) => {}
                            Ok(Err(_)) | Err(_) => break,
                        }
                    }
                    Some(Ok(_)) => {
                        // Ignore Binary / Pong frames.
                    }
                    Some(Err(e)) => {
                        warn!(error = %e, "WebSocket receive error");
                        break;
                    }
                }
            }
            // Branch 2: Broadcast event from the validator.
            event = event_rx.recv() => {
                match event {
                    Ok(ev) if subs.matches(&ev) => {
                        consecutive_lags = 0;

                        // Auto-unsubscribe after SignatureNotification delivery.
                        let auto_unsub_sig = if let PubsubEvent::SignatureNotification {
                            ref signature, ..
                        } = ev
                        {
                            Some(signature.clone())
                        } else {
                            None
                        };

                        // Serialize the event (each subscriber serializes its own copy).
                        match serde_json::to_string(&ev) {
                            Ok(json) => {
                                let send_result = tokio::time::timeout(
                                    WS_SEND_TIMEOUT,
                                    socket.send(Message::Text(json.into())),
                                )
                                .await;
                                match send_result {
                                    Ok(Ok(())) => {
                                        metrics::counter!("nusantara_rpc_ws_events_sent")
                                            .increment(1);
                                        if let Some(sig) = auto_unsub_sig {
                                            subs.signatures.remove(&sig);
                                        }
                                    }
                                    Ok(Err(_)) => {
                                        debug!("WebSocket send failed, closing");
                                        break;
                                    }
                                    Err(_) => {
                                        warn!("WebSocket send timed out, closing connection");
                                        metrics::counter!("nusantara_rpc_ws_send_timeout")
                                            .increment(1);
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to serialize pubsub event");
                            }
                        }
                    }
                    Ok(_) => {
                        consecutive_lags = 0;
                        // Event does not match any active subscription.
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        consecutive_lags += 1;
                        warn!(
                            missed = n,
                            consecutive_lags,
                            "WebSocket subscriber lagged, events dropped"
                        );
                        metrics::counter!("nusantara_rpc_ws_events_lagged").increment(n);
                        // Drop the connection on second consecutive lag (F9).
                        if consecutive_lags >= 2 {
                            warn!("closing WebSocket: lagged twice consecutively");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("pubsub channel closed, terminating WebSocket");
                        break;
                    }
                }
            }
        }
    }

    debug!("WebSocket session ended");
    // _guard drops here → gauge decremented.
}

// ---------------------------------------------------------------------------
// Client message handling
// ---------------------------------------------------------------------------

async fn handle_client_message(text: &str, subs: &mut Subscriptions, socket: &mut WebSocket) {
    let req: ClientRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            let err_msg = serde_json::json!({"error": format!("invalid request: {e}")});
            let _ = tokio::time::timeout(
                WS_SEND_TIMEOUT,
                socket.send(Message::Text(err_msg.to_string().into())),
            )
            .await;
            return;
        }
    };

    let (ack, recognized) = match req.method.as_str() {
        "slotSubscribe" => {
            if !subs.slot && subs.count() >= MAX_SUBSCRIPTIONS_PER_CONN {
                (
                    serde_json::json!({"error": format!(
                        "subscription limit reached ({MAX_SUBSCRIPTIONS_PER_CONN})"
                    )}),
                    false,
                )
            } else {
                subs.slot = true;
                (
                    serde_json::json!({"result": "subscribed", "subscription": "slot"}),
                    true,
                )
            }
        }
        "slotUnsubscribe" => {
            subs.slot = false;
            (
                serde_json::json!({"result": "unsubscribed", "subscription": "slot"}),
                true,
            )
        }
        "blockSubscribe" => {
            if !subs.block && subs.count() >= MAX_SUBSCRIPTIONS_PER_CONN {
                (
                    serde_json::json!({"error": format!(
                        "subscription limit reached ({MAX_SUBSCRIPTIONS_PER_CONN})"
                    )}),
                    false,
                )
            } else {
                subs.block = true;
                (
                    serde_json::json!({"result": "subscribed", "subscription": "block"}),
                    true,
                )
            }
        }
        "blockUnsubscribe" => {
            subs.block = false;
            (
                serde_json::json!({"result": "unsubscribed", "subscription": "block"}),
                true,
            )
        }
        "signatureSubscribe" => {
            if let Some(params) = &req.params {
                if params.signature.is_empty() {
                    (
                        serde_json::json!({"error": "missing signature parameter"}),
                        false,
                    )
                } else if !subs.signatures.contains(&params.signature)
                    && subs.count() >= MAX_SUBSCRIPTIONS_PER_CONN
                {
                    (
                        serde_json::json!({"error": format!(
                            "subscription limit reached ({MAX_SUBSCRIPTIONS_PER_CONN})"
                        )}),
                        false,
                    )
                } else {
                    subs.signatures.insert(params.signature.clone());
                    (
                        serde_json::json!({"result": "subscribed", "subscription": "signature"}),
                        true,
                    )
                }
            } else {
                (
                    serde_json::json!({"error": "missing params.signature for signatureSubscribe"}),
                    false,
                )
            }
        }
        "signatureUnsubscribe" => {
            if let Some(params) = &req.params {
                subs.signatures.remove(&params.signature);
            }
            (
                serde_json::json!({"result": "unsubscribed", "subscription": "signature"}),
                true,
            )
        }
        _ => (
            serde_json::json!({"error": format!("unknown method: {}", req.method)}),
            false,
        ),
    };

    if recognized {
        metrics::counter!("nusantara_rpc_ws_subscriptions", "method" => req.method.clone())
            .increment(1);
        debug!(method = %req.method, active = subs.has_any(), "subscription updated");
    }

    let _ = tokio::time::timeout(
        WS_SEND_TIMEOUT,
        socket.send(Message::Text(ack.to_string().into())),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriptions_default_has_none() {
        let subs = Subscriptions::default();
        assert!(!subs.has_any());
        assert!(!subs.matches(&PubsubEvent::SlotUpdate {
            slot: 1,
            parent: 0,
            root: 0,
        }));
        assert!(!subs.matches(&PubsubEvent::BlockNotification {
            slot: 1,
            block_hash: "abc".to_string(),
            tx_count: 0,
        }));
        assert!(!subs.matches(&PubsubEvent::SignatureNotification {
            signature: "test_sig".to_string(),
            slot: 1,
            status: "success".to_string(),
        }));
    }

    #[test]
    fn subscriptions_slot_only() {
        let subs = Subscriptions {
            slot: true,
            block: false,
            ..Default::default()
        };
        assert!(subs.has_any());
        assert!(subs.matches(&PubsubEvent::SlotUpdate {
            slot: 1,
            parent: 0,
            root: 0,
        }));
        assert!(!subs.matches(&PubsubEvent::BlockNotification {
            slot: 1,
            block_hash: "abc".to_string(),
            tx_count: 0,
        }));
    }

    #[test]
    fn subscriptions_block_only() {
        let subs = Subscriptions {
            slot: false,
            block: true,
            ..Default::default()
        };
        assert!(subs.has_any());
        assert!(!subs.matches(&PubsubEvent::SlotUpdate {
            slot: 1,
            parent: 0,
            root: 0,
        }));
        assert!(subs.matches(&PubsubEvent::BlockNotification {
            slot: 1,
            block_hash: "abc".to_string(),
            tx_count: 0,
        }));
    }

    #[test]
    fn subscriptions_both() {
        let subs = Subscriptions {
            slot: true,
            block: true,
            ..Default::default()
        };
        assert!(subs.has_any());
        assert!(subs.matches(&PubsubEvent::SlotUpdate {
            slot: 5,
            parent: 4,
            root: 3,
        }));
        assert!(subs.matches(&PubsubEvent::BlockNotification {
            slot: 5,
            block_hash: "xyz".to_string(),
            tx_count: 10,
        }));
    }

    #[test]
    fn pubsub_event_serializes_with_type_tag() {
        let event = PubsubEvent::SlotUpdate {
            slot: 42,
            parent: 41,
            root: 40,
        };
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["type"], "SlotUpdate");
        assert_eq!(json["slot"], 42);
        assert_eq!(json["parent"], 41);
        assert_eq!(json["root"], 40);
    }

    #[test]
    fn pubsub_event_block_serializes_with_type_tag() {
        let event = PubsubEvent::BlockNotification {
            slot: 100,
            block_hash: "deadbeef".to_string(),
            tx_count: 7,
        };
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["type"], "BlockNotification");
        assert_eq!(json["slot"], 100);
        assert_eq!(json["blockHash"], "deadbeef");
        assert_eq!(json["txCount"], 7);
    }

    #[test]
    fn subscriptions_signature_match() {
        let mut subs = Subscriptions::default();
        subs.signatures.insert("sig_abc".to_string());

        assert!(subs.has_any());
        assert!(subs.matches(&PubsubEvent::SignatureNotification {
            signature: "sig_abc".to_string(),
            slot: 5,
            status: "success".to_string(),
        }));
        assert!(!subs.matches(&PubsubEvent::SignatureNotification {
            signature: "sig_other".to_string(),
            slot: 5,
            status: "success".to_string(),
        }));
        assert!(!subs.matches(&PubsubEvent::SlotUpdate {
            slot: 1,
            parent: 0,
            root: 0,
        }));
    }

    #[test]
    fn pubsub_event_signature_serializes_with_type_tag() {
        let event = PubsubEvent::SignatureNotification {
            signature: "test_sig".to_string(),
            slot: 77,
            status: "success".to_string(),
        };
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["type"], "SignatureNotification");
        assert_eq!(json["signature"], "test_sig");
        assert_eq!(json["slot"], 77);
        assert_eq!(json["status"], "success");
    }

    #[test]
    fn client_request_deserializes() {
        let raw = r#"{"method": "slotSubscribe"}"#;
        let req: ClientRequest = serde_json::from_str(raw).expect("parse");
        assert_eq!(req.method, "slotSubscribe");
    }

    #[test]
    fn client_request_with_params_deserializes() {
        let raw =
            r#"{"method": "signatureSubscribe", "params": {"signature": "abc123"}}"#;
        let req: ClientRequest = serde_json::from_str(raw).expect("parse");
        assert_eq!(req.method, "signatureSubscribe");
        assert_eq!(req.params.as_ref().unwrap().signature, "abc123");
    }

    #[test]
    fn new_pubsub_channel_creates_working_pair() {
        let tx = RpcState::new_pubsub_channel();
        let mut rx = tx.subscribe();

        let event = PubsubEvent::SlotUpdate {
            slot: 1,
            parent: 0,
            root: 0,
        };
        tx.send(event.clone()).expect("send");

        let received = rx.try_recv().expect("recv");
        match received {
            PubsubEvent::SlotUpdate { slot, parent, root } => {
                assert_eq!(slot, 1);
                assert_eq!(parent, 0);
                assert_eq!(root, 0);
            }
            _ => panic!("unexpected event variant"),
        }
    }

    #[test]
    fn subscriptions_count_empty() {
        let subs = Subscriptions::default();
        assert_eq!(subs.count(), 0);
    }

    #[test]
    fn subscriptions_count_slot_and_block() {
        let subs = Subscriptions {
            slot: true,
            block: true,
            ..Default::default()
        };
        assert_eq!(subs.count(), 2);
    }

    #[test]
    fn subscriptions_count_with_signatures() {
        let mut subs = Subscriptions {
            slot: true,
            ..Default::default()
        };
        subs.signatures.insert("sig1".to_string());
        subs.signatures.insert("sig2".to_string());
        assert_eq!(subs.count(), 3);
    }

    #[test]
    fn ws_send_timeout_positive() {
        assert!(WS_SEND_TIMEOUT.as_secs() > 0);
    }
}
