# Nusantara WebSocket Pubsub API

Real-time event streaming over WebSocket for slot updates, block notifications, and transaction signature confirmations.

## Connection

**Endpoint:** `ws://localhost:8899/v1/ws`

(For TLS-enabled validators: `wss://localhost:8899/v1/ws`)

Connect using any WebSocket client:

```bash
websocat ws://localhost:8899/v1/ws
```

## Protocol

The WebSocket protocol uses simple JSON messages for subscribing, unsubscribing, and receiving events. There is no JSON-RPC framing -- messages are plain JSON objects.

## Subscribe Methods

### slotSubscribe

Subscribe to real-time slot updates.

**Request:**

```json
{"method": "slotSubscribe"}
```

**Acknowledgement:**

```json
{"result": "subscribed", "subscription": "slot"}
```

### blockSubscribe

Subscribe to block notifications emitted when a new block is produced.

**Request:**

```json
{"method": "blockSubscribe"}
```

**Acknowledgement:**

```json
{"result": "subscribed", "subscription": "block"}
```

### signatureSubscribe

Subscribe to a one-shot notification when a specific transaction is confirmed. The subscription is automatically removed after the notification is delivered.

**Request:**

```json
{"method": "signatureSubscribe", "params": {"signature": "BASE64_TX_HASH"}}
```

**Acknowledgement:**

```json
{"result": "subscribed", "subscription": "signature"}
```

## Unsubscribe Methods

### slotUnsubscribe

Stop receiving slot updates.

**Request:**

```json
{"method": "slotUnsubscribe"}
```

**Acknowledgement:**

```json
{"result": "unsubscribed", "subscription": "slot"}
```

### blockUnsubscribe

Stop receiving block notifications.

**Request:**

```json
{"method": "blockUnsubscribe"}
```

**Acknowledgement:**

```json
{"result": "unsubscribed", "subscription": "block"}
```

### signatureUnsubscribe

Stop listening for a specific transaction signature (if not yet delivered).

**Request:**

```json
{"method": "signatureUnsubscribe", "params": {"signature": "BASE64_TX_HASH"}}
```

**Acknowledgement:**

```json
{"result": "unsubscribed", "subscription": "signature"}
```

## Event Types

### SlotUpdate

Emitted each time the validator advances to a new slot.

```json
{
  "type": "SlotUpdate",
  "slot": 42,
  "parent": 41,
  "root": 40
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"SlotUpdate"` |
| `slot` | u64 | Current slot number |
| `parent` | u64 | Parent slot number |
| `root` | u64 | Latest finalized root slot |

### BlockNotification

Emitted when a new block is produced and stored.

```json
{
  "type": "BlockNotification",
  "slot": 42,
  "block_hash": "YmxvY2tfaGFzaF9iYXNlNjQ",
  "tx_count": 7
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"BlockNotification"` |
| `slot` | u64 | Block slot number |
| `block_hash` | string | Block hash (Base64 URL-safe no-pad) |
| `tx_count` | u64 | Number of transactions in the block |

### SignatureNotification

Emitted when a subscribed transaction is included in a block. This is a **one-shot** event -- the subscription is automatically removed after delivery.

```json
{
  "type": "SignatureNotification",
  "signature": "dHhfaGFzaF9iYXNlNjQ",
  "slot": 42,
  "status": "success"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"SignatureNotification"` |
| `signature` | string | Transaction hash (Base64 URL-safe no-pad) |
| `slot` | u64 | Slot the transaction was confirmed in |
| `status` | string | `"success"` or `"failed: <reason>"` |

## Error Responses

Invalid or unknown methods return an error object:

```json
{"error": "unknown method: foo"}
```

Malformed JSON returns:

```json
{"error": "invalid request: <parse error details>"}
```

## Example Session

```
> {"method": "slotSubscribe"}
< {"result": "subscribed", "subscription": "slot"}
< {"type": "SlotUpdate", "slot": 100, "parent": 99, "root": 95}
< {"type": "SlotUpdate", "slot": 101, "parent": 100, "root": 96}
> {"method": "blockSubscribe"}
< {"result": "subscribed", "subscription": "block"}
< {"type": "BlockNotification", "slot": 102, "block_hash": "YmxvY2tfMTAyX2hhc2g", "tx_count": 3}
< {"type": "SlotUpdate", "slot": 103, "parent": 102, "root": 97}
> {"method": "slotUnsubscribe"}
< {"result": "unsubscribed", "subscription": "slot"}
< {"type": "BlockNotification", "slot": 104, "block_hash": "YmxvY2tfMTA0X2hhc2g", "tx_count": 5}
> {"method": "blockUnsubscribe"}
< {"result": "unsubscribed", "subscription": "block"}
```

## Behavior Notes

- **No backfill:** Events are delivered only after the subscription is established. Historical events are not replayed.
- **Buffer capacity:** The broadcast channel holds up to 4,096 events. If a client falls behind and the buffer overflows, lagged events are silently dropped. The server logs a warning but the connection remains open.
- **Ping/Pong keepalive:** Standard WebSocket Ping frames are supported. The server responds with a matching Pong frame to keep the connection alive through proxies and load balancers.
- **Multiple subscriptions:** A single connection can subscribe to `slot`, `block`, and multiple `signature` subscriptions simultaneously. Events are interleaved in delivery order.
- **One-shot signatures:** `signatureSubscribe` subscriptions are automatically removed after delivering the notification, preventing memory leaks for long-lived connections.
- **Connection lifetime:** The connection stays open until the client disconnects, sends a Close frame, or the server shuts down.
- **Metrics:** The server tracks active WebSocket connections (`nusantara_rpc_ws_active_connections`), total events sent (`nusantara_rpc_ws_events_sent`), and lagged events (`nusantara_rpc_ws_events_lagged`).
