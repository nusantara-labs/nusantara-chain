# Nusantara JSON-RPC 2.0 API

JSON-RPC 2.0 compatible interface providing Solana-style RPC access alongside the REST API.

## Endpoint

```
POST http://localhost:8899/rpc
Content-Type: application/json
```

## Protocol

Implements the [JSON-RPC 2.0 specification](https://www.jsonrpc.org/specification). Supports both single requests and batch requests (JSON array of request objects).

### Request Format

```json
{
  "jsonrpc": "2.0",
  "method": "<method_name>",
  "params": [...],
  "id": 1
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `jsonrpc` | string | Yes | Must be `"2.0"` |
| `method` | string | Yes | Method name to invoke |
| `params` | array | No | Positional parameters (method-specific) |
| `id` | any | No | Request identifier, echoed in response. Omit for notifications. |

### Response Format

**Success:**

```json
{
  "jsonrpc": "2.0",
  "result": <value>,
  "id": 1
}
```

**Error:**

```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32601,
    "message": "method not found: foo"
  },
  "id": 1
}
```

---

## Methods

### getHealth

Returns a simple health check string.

**Parameters:** None

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getHealth","id":1}'
```

**Response:**

```json
{"jsonrpc":"2.0","result":"ok","id":1}
```

---

### getSlot

Returns current slot information.

**Parameters:** None

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getSlot","id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "slot": 1042,
    "latest_stored_slot": 1042,
    "latest_root": 1040
  },
  "id": 1
}
```

---

### getLatestBlockhash

Returns the most recent blockhash for transaction signing.

**Parameters:** None

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getLatestBlockhash","id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "blockhash": "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo",
    "slot": 1042
  },
  "id": 1
}
```

---

### getAccountInfo

Returns full account information for the given address.

**Parameters:** `[address_base64]`

| Index | Type | Description |
|-------|------|-------------|
| 0 | string | Account address (Base64 URL-safe no-pad) |

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getAccountInfo","params":["dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz"],"id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "address": "dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz",
    "lamports": 5000000000,
    "nusa": 5.0,
    "owner": "c3lzdGVtX3Byb2dyYW1faWQ",
    "executable": false,
    "rent_epoch": 0,
    "data_len": 0
  },
  "id": 1
}
```

---

### getBalance

Returns the lamport balance for the given address.

**Parameters:** `[address_base64]`

| Index | Type | Description |
|-------|------|-------------|
| 0 | string | Account address (Base64 URL-safe no-pad) |

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getBalance","params":["dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz"],"id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "value": 5000000000
  },
  "id": 1
}
```

---

### sendTransaction

Submit a signed, serialized transaction.

**Parameters:** `[base64_borsh_transaction]`

| Index | Type | Description |
|-------|------|-------------|
| 0 | string | Base64 URL-safe no-pad encoded Borsh-serialized `Transaction` |

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"sendTransaction","params":["AGFiY2RlZmdoaWprbG1ub3BxcnN0"],"id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": "dHhfc2lnbmF0dXJlX2Jhc2U2NA",
  "id": 1
}
```

The result is the transaction signature as a Base64 string.

---

### getTransaction

Returns transaction status and metadata.

**Parameters:** `[hash_base64]`

| Index | Type | Description |
|-------|------|-------------|
| 0 | string | Transaction hash (Base64 URL-safe no-pad) |

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getTransaction","params":["dHhfaGFzaF9iYXNlNjQ"],"id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "signature": "dHhfaGFzaF9iYXNlNjQ",
    "slot": 42,
    "status": "success",
    "fee": 5000,
    "pre_balances": [10000000000, 0],
    "post_balances": [9999995000, 5000000000],
    "compute_units_consumed": 150
  },
  "id": 1
}
```

---

### getBlock

Returns block header information for the given slot.

**Parameters:** `[slot]`

| Index | Type | Description |
|-------|------|-------------|
| 0 | u64 | Slot number |

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getBlock","params":[42],"id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "slot": 42,
    "parent_slot": 41,
    "parent_hash": "cGFyZW50X2hhc2hfYmFzZTY0",
    "block_hash": "YmxvY2tfaGFzaF9iYXNlNjQ",
    "timestamp": 1700000000,
    "validator": "dmFsaWRhdG9yX2lkZW50aXR5",
    "transaction_count": 7,
    "merkle_root": "bWVya2xlX3Jvb3RfaGFzaA"
  },
  "id": 1
}
```

---

### getEpochInfo

Returns information about the current epoch.

**Parameters:** None

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getEpochInfo","id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "epoch": 2,
    "slot_index": 1042,
    "slots_in_epoch": 432000,
    "absolute_slot": 865042,
    "timestamp": 1700000000,
    "leader_schedule_epoch": 3
  },
  "id": 1
}
```

---

### getLeaderSchedule

Returns the leader schedule for a given epoch or the current epoch.

**Parameters:** `[epoch?]` (optional)

| Index | Type | Description |
|-------|------|-------------|
| 0 | u64 (optional) | Epoch number. Defaults to the current epoch if omitted. |

**Example (current epoch):**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getLeaderSchedule","id":1}'
```

**Example (specific epoch):**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getLeaderSchedule","params":[3],"id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "epoch": 3,
    "schedule": [
      {"slot": 1296000, "leader": "dmFsaWRhdG9yXzE"},
      {"slot": 1296001, "leader": "dmFsaWRhdG9yXzE"},
      {"slot": 1296002, "leader": "dmFsaWRhdG9yXzI"}
    ]
  },
  "id": 1
}
```

---

### getVoteAccounts

Returns all validators with their vote accounts and stake information.

**Parameters:** None

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getVoteAccounts","id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "total_active_stake": 1000000000000,
    "validators": [
      {
        "identity": "dmFsaWRhdG9yXzFfaWQ",
        "vote_account": "dm90ZV9hY2NvdW50XzE",
        "commission": 10,
        "active_stake": 500000000000,
        "last_vote": 1042,
        "root_slot": 1040
      }
    ]
  },
  "id": 1
}
```

---

### getProgramAccounts

Returns all accounts owned by the given program.

**Parameters:** `[program_base64]`

| Index | Type | Description |
|-------|------|-------------|
| 0 | string | Program address (Base64 URL-safe no-pad) |

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getProgramAccounts","params":["c3Rha2VfcHJvZ3JhbV9pZA"],"id":1}'
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "result": {
    "accounts": [
      {
        "address": "YWNjb3VudF8x",
        "lamports": 10000000000,
        "nusa": 10.0,
        "owner": "c3Rha2VfcHJvZ3JhbV9pZA",
        "executable": false,
        "data_len": 200,
        "rent_epoch": 0
      }
    ],
    "count": 1
  },
  "id": 1
}
```

Note: Results are limited to 1,000 accounts maximum.

---

## Batch Requests

Send multiple requests in a single HTTP call by wrapping them in a JSON array. Responses are returned in the same order.

**Example:**

```bash
curl -X POST http://localhost:8899/rpc \
  -H "Content-Type: application/json" \
  -d '[
    {"jsonrpc":"2.0","method":"getSlot","id":1},
    {"jsonrpc":"2.0","method":"getHealth","id":2},
    {"jsonrpc":"2.0","method":"getEpochInfo","id":3}
  ]'
```

**Response:**

```json
[
  {"jsonrpc":"2.0","result":{"slot":1042,"latest_stored_slot":1042,"latest_root":1040},"id":1},
  {"jsonrpc":"2.0","result":"ok","id":2},
  {"jsonrpc":"2.0","result":{"epoch":2,"slot_index":1042,"slots_in_epoch":432000,"absolute_slot":865042,"timestamp":1700000000,"leader_schedule_epoch":3},"id":3}
]
```

An empty batch array returns an error:

```json
{"jsonrpc":"2.0","error":{"code":-32600,"message":"empty batch"},"id":null}
```

---

## Error Codes

Standard JSON-RPC 2.0 error codes:

| Code | Name | Description |
|------|------|-------------|
| -32700 | Parse Error | The request body is not valid JSON |
| -32600 | Invalid Request | Missing required fields, wrong `jsonrpc` version, or empty batch |
| -32601 | Method Not Found | The requested method name does not exist |
| -32602 | Invalid Params | Wrong parameter type, missing required parameter, or invalid Base64 |
| -32603 | Internal Error | Server-side failure (storage error, mempool rejection, etc.) |

**Error response example:**

```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32601,
    "message": "method not found: getUnknown"
  },
  "id": 1
}
```

---

## Notes

- The `id` field can be a number, string, or null. It is echoed back in the response for client-side correlation.
- Notifications (requests without `id`) are processed but do not generate a response.
- All hashes and addresses in both parameters and results use Base64 URL-safe no-pad encoding.
- The JSON-RPC endpoint shares the same storage, bank, and mempool as the REST API. Both interfaces can be used interchangeably.
