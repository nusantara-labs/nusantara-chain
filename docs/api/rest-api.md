# Nusantara REST API Reference

**Base URL:** `http://localhost:8899`

**Swagger UI:** `http://localhost:8899/swagger-ui/`

## Conventions

- All responses are JSON (`Content-Type: application/json`) unless otherwise noted.
- Hashes, addresses, and public keys are encoded as **Base64 URL-safe no-pad** strings.
- Transactions are submitted as Base64 URL-safe no-pad encoded Borsh-serialized bytes.
- Amounts are expressed in lamports (1 NUSA = 1,000,000,000 lamports). Some responses also include a `nusa` field with the floating-point equivalent.
- The interactive OpenAPI / Swagger UI is available at `/swagger-ui/` for exploring endpoints in a browser.
- CORS is permissive by default (all origins allowed).

## Error Format

All errors return a JSON body with a single `error` field:

```json
{
  "error": "description of what went wrong"
}
```

**HTTP Status Codes:**

| Status | Meaning |
|--------|---------|
| 200 | Success |
| 400 | Bad Request -- invalid input, malformed address, etc. |
| 404 | Not Found -- account, block, transaction, or program does not exist |
| 429 | Too Many Requests -- rate limited |
| 500 | Internal Server Error -- storage or server-side failure |
| 503 | Service Unavailable -- faucet is disabled |

---

## Health & Status

### GET /v1/health

Returns the overall health of the validator node.

**Health status values:**
- `"ok"` -- node is healthy and has peers
- `"degraded"` -- node has no connected peers
- `"behind"` -- node is more than 100 slots behind the root

**Example:**

```bash
curl http://localhost:8899/v1/health
```

**Response:**

```json
{
  "status": "ok",
  "slot": 1042,
  "identity": "dGhpcyBpcyBhIGJhc2U2NCBleGFtcGxlIGlkZW50aXR5",
  "root_slot": 1040,
  "behind_slots": 2,
  "peer_count": 3,
  "epoch": 0,
  "epoch_progress_pct": 0.24,
  "consecutive_skips": 0,
  "total_active_stake": 500000000000
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `status` | string | `"ok"`, `"degraded"`, or `"behind"` |
| `slot` | u64 | Current slot the bank is processing |
| `identity` | string | Validator identity (Base64) |
| `root_slot` | u64 | Latest finalized root slot |
| `behind_slots` | u64 | Difference between current slot and root |
| `peer_count` | usize | Number of connected gossip peers |
| `epoch` | u64 | Current epoch number |
| `epoch_progress_pct` | f64 | Percentage through the current epoch |
| `consecutive_skips` | u64 | Number of consecutive slots this validator skipped |
| `total_active_stake` | u64 | Total active stake across all validators (lamports) |

---

### GET /v1/slot

Returns current slot information.

**Example:**

```bash
curl http://localhost:8899/v1/slot
```

**Response:**

```json
{
  "slot": 1042,
  "latest_stored_slot": 1042,
  "latest_root": 1040
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `slot` | u64 | Current slot the bank is processing |
| `latest_stored_slot` | u64 or null | Most recent slot written to storage |
| `latest_root` | u64 or null | Most recent finalized root slot |

---

### GET /v1/blockhash

Returns the latest blockhash for transaction signing.

**Example:**

```bash
curl http://localhost:8899/v1/blockhash
```

**Response:**

```json
{
  "blockhash": "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY3ODkwYWJjZGVmZ2hpamtsbW5vcHFyc3R1dg",
  "slot": 1042
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `blockhash` | string | Recent blockhash (Base64), used for transaction signing |
| `slot` | u64 | Slot the blockhash came from |

---

## Accounts

### GET /v1/account/{address}

Returns account information for the given address.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `address` | string | Base64 URL-safe no-pad encoded account address |

**Example:**

```bash
curl http://localhost:8899/v1/account/dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz
```

**Response:**

```json
{
  "address": "dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz",
  "lamports": 5000000000,
  "nusa": 5.0,
  "owner": "c3lzdGVtX3Byb2dyYW1faWQ",
  "executable": false,
  "rent_epoch": 0,
  "data_len": 0
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Account address (Base64) |
| `lamports` | u64 | Balance in lamports |
| `nusa` | f64 | Balance in NUSA |
| `owner` | string | Owner program address (Base64) |
| `executable` | bool | Whether this account contains executable code |
| `rent_epoch` | u64 | Next epoch rent will be collected |
| `data_len` | usize | Size of account data in bytes |

**Errors:**

- `404` -- Account not found

---

### GET /v1/accounts/by-owner/{owner}

Returns all accounts owned by the given program address.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `owner` | string | Base64 URL-safe no-pad owner (program) address |

**Query Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `limit` | usize | 100 | Maximum number of results (max 1000) |

**Example:**

```bash
curl "http://localhost:8899/v1/accounts/by-owner/c3lzdGVtX3Byb2dyYW1faWQ?limit=10"
```

**Response:**

```json
{
  "accounts": [
    {
      "address": "YWNjb3VudF8x",
      "lamports": 1000000000,
      "nusa": 1.0,
      "owner": "c3lzdGVtX3Byb2dyYW1faWQ",
      "executable": false,
      "data_len": 0,
      "rent_epoch": 0
    }
  ],
  "count": 1
}
```

**Errors:**

- `400` -- Invalid owner address

---

### GET /v1/accounts/by-program/{program}

Returns all accounts belonging to the given program. Functionally identical to `by-owner` but semantically expresses program ownership.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `program` | string | Base64 URL-safe no-pad program address |

**Query Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `limit` | usize | 100 | Maximum number of results (max 1000) |

**Example:**

```bash
curl "http://localhost:8899/v1/accounts/by-program/c3Rha2VfcHJvZ3JhbV9pZA?limit=50"
```

**Response:** Same schema as `GET /v1/accounts/by-owner/{owner}`.

**Errors:**

- `400` -- Invalid program address

---

### GET /v1/account/{address}/proof

Returns account data together with a Merkle proof against the current state root. Light clients can use this to verify that an account exists in the validator's committed state without downloading the full ledger.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `address` | string | Base64 URL-safe no-pad account address |

**Example:**

```bash
curl http://localhost:8899/v1/account/dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz/proof
```

**Response:**

```json
{
  "address": "dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz",
  "lamports": 5000000000,
  "owner": "c3lzdGVtX3Byb2dyYW1faWQ",
  "executable": false,
  "data_len": 0,
  "proof": {
    "siblings": [
      "c2libGluZzFoYXNo",
      "c2libGluZzJoYXNo"
    ],
    "path": [true, false],
    "leaf_index": 42,
    "total_leaves": 256
  },
  "state_root": "c3RhdGVfcm9vdF9oYXNo",
  "slot": 1042
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Account address (Base64) |
| `lamports` | u64 | Balance in lamports |
| `owner` | string | Owner program (Base64) |
| `executable` | bool | Whether executable |
| `data_len` | usize | Account data size in bytes |
| `proof` | ProofData | Merkle proof data (see below) |
| `state_root` | string | State Merkle root (Base64) |
| `slot` | u64 | Slot when the proof was generated |

**ProofData Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `siblings` | string[] | Base64-encoded sibling hashes from leaf to root |
| `path` | bool[] | true if node was right child at each level |
| `leaf_index` | usize | Index of the leaf in the sorted array |
| `total_leaves` | usize | Total leaves when proof was generated |

**Errors:**

- `400` -- Invalid address
- `404` -- Account not found or no state proof available

---

## Blocks

### GET /v1/block/{slot}

Returns block header information for the given slot.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `slot` | u64 | Slot number |

**Example:**

```bash
curl http://localhost:8899/v1/block/42
```

**Response:**

```json
{
  "slot": 42,
  "parent_slot": 41,
  "parent_hash": "cGFyZW50X2hhc2hfYmFzZTY0",
  "block_hash": "YmxvY2tfaGFzaF9iYXNlNjQ",
  "timestamp": 1700000000,
  "validator": "dmFsaWRhdG9yX2lkZW50aXR5",
  "transaction_count": 7,
  "merkle_root": "bWVya2xlX3Jvb3RfaGFzaA"
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `slot` | u64 | Slot number |
| `parent_slot` | u64 | Parent block's slot |
| `parent_hash` | string | Parent block hash (Base64) |
| `block_hash` | string | This block's hash (Base64) |
| `timestamp` | i64 | Unix timestamp (seconds) |
| `validator` | string | Block producer identity (Base64) |
| `transaction_count` | u64 | Number of transactions in the block |
| `merkle_root` | string | Transaction Merkle root (Base64) |

**Errors:**

- `404` -- Block at the given slot not found

---

## Transactions

### GET /v1/transaction/{hash}

Returns the status and metadata of a confirmed transaction.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `hash` | string | Transaction hash / signature (Base64) |

**Example:**

```bash
curl http://localhost:8899/v1/transaction/dHhfaGFzaF9iYXNlNjQ
```

**Response:**

```json
{
  "signature": "dHhfaGFzaF9iYXNlNjQ",
  "slot": 42,
  "status": "success",
  "fee": 5000,
  "pre_balances": [10000000000, 0],
  "post_balances": [9999995000, 5000000000],
  "compute_units_consumed": 150
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `signature` | string | Transaction signature (Base64) |
| `slot` | u64 | Slot the transaction was included in |
| `status` | string | `"success"` or `"failed: <reason>"` |
| `fee` | u64 | Fee paid in lamports |
| `pre_balances` | u64[] | Account balances before execution |
| `post_balances` | u64[] | Account balances after execution |
| `compute_units_consumed` | u64 | Compute units used |

**Errors:**

- `404` -- Transaction not found

---

### POST /v1/transaction/send

Submit a signed transaction to the mempool.

**Request Body:**

```json
{
  "transaction": "<base64-borsh-encoded-transaction>"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `transaction` | string | Base64 URL-safe no-pad encoded Borsh-serialized `Transaction` |

**Example:**

```bash
curl -X POST http://localhost:8899/v1/transaction/send \
  -H "Content-Type: application/json" \
  -d '{"transaction": "AGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6MTIzNDU2Nzg5MA"}'
```

**Response:**

```json
{
  "signature": "dHhfc2lnbmF0dXJlX2Jhc2U2NA"
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `signature` | string | Transaction hash / signature (Base64) |

**Errors:**

- `400` -- Invalid Base64, invalid Borsh, or malformed transaction
- `500` -- Mempool rejected the transaction

**Transaction Building Flow:**

1. Fetch a recent blockhash via `GET /v1/blockhash`
2. Build instructions and create a `Message` with the payer
3. Set `message.recent_blockhash` to the fetched blockhash
4. Create a `Transaction` from the message and sign it
5. Borsh-serialize the transaction and Base64-encode the bytes
6. Submit via this endpoint

---

## Epoch & Leader Schedule

### GET /v1/epoch-info

Returns information about the current epoch.

**Example:**

```bash
curl http://localhost:8899/v1/epoch-info
```

**Response:**

```json
{
  "epoch": 2,
  "slot_index": 1042,
  "slots_in_epoch": 432000,
  "absolute_slot": 865042,
  "timestamp": 1700000000,
  "leader_schedule_epoch": 3
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `epoch` | u64 | Current epoch number |
| `slot_index` | u64 | Slot position within the current epoch |
| `slots_in_epoch` | u64 | Total slots per epoch (432,000) |
| `absolute_slot` | u64 | Absolute slot number from genesis |
| `timestamp` | i64 | Unix timestamp (seconds) |
| `leader_schedule_epoch` | u64 | Epoch for which the leader schedule is active |

---

### GET /v1/leader-schedule

Returns the leader schedule for the current epoch.

**Example:**

```bash
curl http://localhost:8899/v1/leader-schedule
```

**Response:**

```json
{
  "epoch": 2,
  "schedule": [
    { "slot": 864000, "leader": "dmFsaWRhdG9yXzE" },
    { "slot": 864001, "leader": "dmFsaWRhdG9yXzE" },
    { "slot": 864002, "leader": "dmFsaWRhdG9yXzI" }
  ]
}
```

---

### GET /v1/leader-schedule/{epoch}

Returns the leader schedule for a specific epoch.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `epoch` | u64 | Epoch number |

**Example:**

```bash
curl http://localhost:8899/v1/leader-schedule/3
```

**Response:** Same schema as `GET /v1/leader-schedule`.

**Response Fields (schedule entries):**

| Field | Type | Description |
|-------|------|-------------|
| `slot` | u64 | Slot number |
| `leader` | string | Leader identity for this slot (Base64) |

---

## Validators & Staking

### GET /v1/validators

Returns the list of all validators sorted by active stake (descending).

**Example:**

```bash
curl http://localhost:8899/v1/validators
```

**Response:**

```json
{
  "total_active_stake": 1000000000000,
  "validators": [
    {
      "identity": "dmFsaWRhdG9yXzFfaWRlbnRpdHk",
      "vote_account": "dm90ZV9hY2NvdW50XzE",
      "commission": 10,
      "active_stake": 500000000000,
      "last_vote": 1042,
      "root_slot": 1040
    },
    {
      "identity": "dmFsaWRhdG9yXzJfaWRlbnRpdHk",
      "vote_account": "dm90ZV9hY2NvdW50XzI",
      "commission": 5,
      "active_stake": 500000000000,
      "last_vote": 1041,
      "root_slot": 1039
    }
  ]
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `total_active_stake` | u64 | Total active stake across all validators (lamports) |
| `validators` | array | Validator entries (see below) |

**Validator Entry Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `identity` | string | Node identity (Base64) |
| `vote_account` | string | Vote account address (Base64) |
| `commission` | u8 | Commission percentage (0-100) |
| `active_stake` | u64 | Active stake delegated to this validator (lamports) |
| `last_vote` | u64 or null | Slot of the most recent vote |
| `root_slot` | u64 or null | Most recent root slot from this validator |

---

### GET /v1/stake-account/{address}

Returns detailed information about a stake account.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `address` | string | Base64 URL-safe no-pad stake account address |

**Example:**

```bash
curl http://localhost:8899/v1/stake-account/c3Rha2VfYWNjb3VudF9hZGRyZXNz
```

**Response:**

```json
{
  "address": "c3Rha2VfYWNjb3VudF9hZGRyZXNz",
  "lamports": 10000000000,
  "state": "Delegated",
  "staker": "c3Rha2VyX2FkZHJlc3M",
  "withdrawer": "d2l0aGRyYXdlcl9hZGRyZXNz",
  "voter": "dm90ZXJfYWRkcmVzcw",
  "stake": 10000000000,
  "activation_epoch": 2,
  "deactivation_epoch": null,
  "rent_exempt_reserve": 2282880
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Stake account address (Base64) |
| `lamports` | u64 | Total balance in lamports |
| `state` | string | Stake state: `"Initialized"`, `"Delegated"`, `"Deactivating"`, `"Uninitialized"` |
| `staker` | string or null | Authorized staker address (Base64) |
| `withdrawer` | string or null | Authorized withdrawer address (Base64) |
| `voter` | string or null | Delegated vote account address (Base64) |
| `stake` | u64 or null | Amount of active stake (lamports) |
| `activation_epoch` | u64 or null | Epoch when stake was activated |
| `deactivation_epoch` | u64 or null | Epoch when deactivation was requested (null if active) |
| `rent_exempt_reserve` | u64 or null | Lamports reserved for rent exemption |

**Errors:**

- `404` -- Stake account not found

---

### GET /v1/vote-account/{address}

Returns detailed information about a vote account.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `address` | string | Base64 URL-safe no-pad vote account address |

**Example:**

```bash
curl http://localhost:8899/v1/vote-account/dm90ZV9hY2NvdW50X2FkZHJlc3M
```

**Response:**

```json
{
  "address": "dm90ZV9hY2NvdW50X2FkZHJlc3M",
  "lamports": 10000000,
  "node_pubkey": "bm9kZV9pZGVudGl0eQ",
  "authorized_voter": "dm90ZXJfYWRkcmVzcw",
  "authorized_withdrawer": "d2l0aGRyYXdlcl9hZGRyZXNz",
  "commission": 10,
  "root_slot": 1040,
  "last_vote_slot": 1042,
  "epoch_credits": [
    { "epoch": 0, "credits": 432, "prev_credits": 0 },
    { "epoch": 1, "credits": 864, "prev_credits": 432 }
  ]
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Vote account address (Base64) |
| `lamports` | u64 | Balance in lamports |
| `node_pubkey` | string | Validator node identity (Base64) |
| `authorized_voter` | string | Authorized voter address (Base64) |
| `authorized_withdrawer` | string | Authorized withdrawer address (Base64) |
| `commission` | u8 | Commission percentage (0-100) |
| `root_slot` | u64 or null | Most recent root slot |
| `last_vote_slot` | u64 or null | Slot of the most recent vote |
| `epoch_credits` | array | Per-epoch credit history (see below) |

**Epoch Credit Entry Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `epoch` | u64 | Epoch number |
| `credits` | u64 | Cumulative credits at end of epoch |
| `prev_credits` | u64 | Credits at start of epoch |

**Errors:**

- `404` -- Vote account not found

---

## Signatures

### GET /v1/signatures/{address}

Returns transaction signatures associated with the given account address.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `address` | string | Base64 URL-safe no-pad account address |

**Example:**

```bash
curl http://localhost:8899/v1/signatures/dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz
```

**Response:**

```json
{
  "address": "dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz",
  "signatures": [
    { "signature": "c2lnXzE", "slot": 42, "tx_index": 0 },
    { "signature": "c2lnXzI", "slot": 43, "tx_index": 1 }
  ]
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Account address (Base64) |
| `signatures` | array | List of signature entries |

**Signature Entry Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `signature` | string | Transaction signature (Base64) |
| `slot` | u64 | Slot the transaction was included in |
| `tx_index` | u32 | Index within the block |

---

## Programs

### GET /v1/program/{address}

Returns information about a deployed program.

**Path Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `address` | string | Base64 URL-safe no-pad program address |

**Example:**

```bash
curl http://localhost:8899/v1/program/cHJvZ3JhbV9hZGRyZXNz
```

**Response:**

```json
{
  "address": "cHJvZ3JhbV9hZGRyZXNz",
  "executable": true,
  "program_data_address": "cHJvZ3JhbV9kYXRhX2FkZHJlc3M",
  "authority": "dXBncmFkZV9hdXRob3JpdHk",
  "deploy_slot": 100,
  "bytecode_size": 65536,
  "lamports": 1000000000
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Program account address (Base64) |
| `executable` | bool | Always `true` for programs |
| `program_data_address` | string | Address of the program data account (Base64) |
| `authority` | string or null | Upgrade authority address (Base64), null if immutable |
| `deploy_slot` | u64 | Slot when the program was deployed |
| `bytecode_size` | usize | Size of the WASM bytecode in bytes |
| `lamports` | u64 | Balance of the program account in lamports |

**Errors:**

- `400` -- Account is not executable or not a valid program
- `404` -- Program not found

---

## Faucet

### POST /v1/airdrop

Request a testnet airdrop. Requires the validator to be started with `--enable-faucet`.

**Request Body:**

```json
{
  "address": "<base64-address>",
  "lamports": 1000000000
}
```

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Recipient address (Base64) |
| `lamports` | u64 | Amount to airdrop in lamports (max 10 NUSA = 10,000,000,000) |

**Example:**

```bash
curl -X POST http://localhost:8899/v1/airdrop \
  -H "Content-Type: application/json" \
  -d '{"address": "dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz", "lamports": 1000000000}'
```

**Response:**

```json
{
  "signature": "YWlyZHJvcF90eF9zaWduYXR1cmU"
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `signature` | string | Airdrop transaction signature (Base64) |

**Errors:**

- `400` -- Invalid address, zero lamports, or exceeds max (10 NUSA)
- `503` -- Faucet is disabled (validator not started with `--enable-faucet`)

---

## Snapshots

### GET /v1/snapshot/latest

Returns metadata about the most recent snapshot.

**Example:**

```bash
curl http://localhost:8899/v1/snapshot/latest
```

**Response:**

```json
{
  "slot": 1000,
  "bank_hash": "YmFua19oYXNoX2Jhc2U2NA",
  "account_count": 1500,
  "timestamp": 1700000000,
  "file_hash": "c25hcHNob3RfZmlsZV9oYXNo",
  "file_size": 52428800
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `slot` | u64 | Slot when the snapshot was taken |
| `bank_hash` | string | Bank hash at snapshot slot (Base64) |
| `account_count` | u64 | Number of accounts in the snapshot |
| `timestamp` | i64 | Unix timestamp (seconds) |
| `file_hash` | string or null | SHA3-512 hash of the snapshot file (Base64), null if file not found |
| `file_size` | u64 or null | Snapshot file size in bytes, null if file not found |

**Errors:**

- `404` -- No snapshot available

---

### GET /v1/snapshot/download

Download the latest snapshot as a binary file stream.

**Example:**

```bash
curl -o snapshot.bin http://localhost:8899/v1/snapshot/download
```

**Response:**

- `Content-Type: application/octet-stream`
- `Content-Disposition: attachment; filename="snapshot-{slot}.bin"`
- Body: raw binary snapshot data

**Errors:**

- `404` -- No snapshot file available
- `500` -- Failed to open snapshot file
