# Nusantara CLI Reference

Complete command reference for the `nusantara` CLI binary.

## Installation

The CLI is built from the `cli/` crate in the workspace:

```bash
cargo build --release -p nusantara-cli
```

The binary is named `nusantara` and will be available at `target/release/nusantara`.

## Global Options

```
nusantara [OPTIONS] <COMMAND>
```

| Option | Short | Description |
|--------|-------|-------------|
| `--url <URL>` | `-u` | RPC endpoint URL (overrides config file) |
| `--keypair <PATH>` | `-k` | Path to keypair file (overrides config file) |
| `--output <FORMAT>` | `-o` | Output format: `text` (default) or `json` |

Global options can be placed before any subcommand and apply to all commands.

**Examples:**

```bash
nusantara --url http://localhost:8899 balance
nusantara -u http://devnet.nusantara.io:8899 -k ~/.config/nusantara/devnet.key slot
nusantara --output json validators
```

---

## Configuration

Configuration is stored at `~/.config/nusantara/cli.toml`.

**Default configuration:**

```toml
rpc_url = "http://127.0.0.1:8899"
keypair_path = "~/.config/nusantara/id.key"
```

### config get

Display the current configuration.

```bash
nusantara config get
```

**Example output:**

```
RPC URL:      http://127.0.0.1:8899
Keypair path: /Users/alice/.config/nusantara/id.key
```

### config set

Update configuration values. Only the specified fields are changed; others remain unchanged.

```bash
nusantara config set [--url <URL>] [--keypair <PATH>]
```

| Option | Description |
|--------|-------------|
| `--url <URL>` | Set the default RPC URL |
| `--keypair <PATH>` | Set the default keypair file path |

**Examples:**

```bash
nusantara config set --url http://devnet.nusantara.io:8899
nusantara config set --keypair ~/.config/nusantara/devnet.key
nusantara config set --url http://localhost:8899 --keypair ~/.config/nusantara/id.key
```

**Example output:**

```
Config updated:
  RPC URL:      http://devnet.nusantara.io:8899
  Keypair path: /Users/alice/.config/nusantara/id.key
```

---

## Keypair Management

### keygen

Generate a new Dilithium3 keypair.

```bash
nusantara keygen [-o <PATH>]
```

| Option | Short | Description |
|--------|-------|-------------|
| `--outfile <PATH>` | `-o` | Output file path (default: `~/.config/nusantara/id.key`) |

The keypair file contains the raw public key (1,952 bytes) followed by the secret key (4,032 bytes) for a total of 5,984 bytes.

**Example:**

```bash
nusantara keygen
```

**Output:**

```
Keypair generated:
  Address: dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz
  Saved to: /Users/alice/.config/nusantara/id.key
```

**Example with custom path:**

```bash
nusantara keygen -o ~/.config/nusantara/validator.key
```

**Output:**

```
Keypair generated:
  Address: YW5vdGhlcl9iYXNlNjRfYWRkcmVzcw
  Saved to: /Users/alice/.config/nusantara/validator.key
```

---

## Account Operations

### balance

Check the NUSA balance of an account.

```bash
nusantara balance [ADDRESS]
```

| Argument | Required | Description |
|----------|----------|-------------|
| `ADDRESS` | No | Account address (Base64). Defaults to the configured keypair's address. |

**Example (own balance):**

```bash
nusantara balance
```

**Output:**

```
5.0 NUSA (5000000000 lamports)
```

**Example (specific address):**

```bash
nusantara balance dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz
```

**JSON output:**

```bash
nusantara --output json balance
```

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

---

### account

View full account information.

```bash
nusantara account <ADDRESS>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `ADDRESS` | Yes | Account address (Base64) |

**Example:**

```bash
nusantara account dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz
```

**Output:**

```
Address:    dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz
Balance:    5.0 NUSA (5000000000 lamports)
Owner:      c3lzdGVtX3Byb2dyYW1faWQ
Executable: false
Data size:  0 bytes
Rent epoch: 0
```

---

### transfer

Transfer NUSA to another account.

```bash
nusantara transfer <TO> <AMOUNT>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `TO` | Yes | Recipient address (Base64) |
| `AMOUNT` | Yes | Amount in NUSA (floating point) |

The transaction is signed with the configured keypair and submitted to the validator.

**Example:**

```bash
nusantara transfer dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz 2.5
```

**Output:**

```
Transfer sent: 2.5 NUSA to dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz
Signature: dHhfc2lnbmF0dXJlX2Jhc2U2NA
```

**JSON output:**

```bash
nusantara --output json transfer dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz 2.5
```

```json
{"signature": "dHhfc2lnbmF0dXJlX2Jhc2U2NA"}
```

---

### airdrop

Request a testnet airdrop. Requires the validator to have `--enable-faucet`. Maximum 10 NUSA per request.

```bash
nusantara airdrop <AMOUNT> [--recipient <ADDRESS>]
```

| Argument | Required | Description |
|----------|----------|-------------|
| `AMOUNT` | Yes | Amount in NUSA (floating point, max 10) |
| `--recipient <ADDRESS>` | No | Recipient address (Base64). Defaults to the configured keypair's address. |

**Example:**

```bash
nusantara airdrop 5
```

**Output:**

```
Airdrop: 5 NUSA to dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz
Signature: YWlyZHJvcF90eF9zaWduYXR1cmU
```

**Example (specific recipient):**

```bash
nusantara airdrop 1.0 --recipient cmVjaXBpZW50X2FkZHJlc3M
```

**JSON output:**

```bash
nusantara --output json airdrop 5
```

```json
{
  "signature": "YWlyZHJvcF90eF9zaWduYXR1cmU",
  "address": "dGhpcyBpcyBhIGJhc2U2NCBhZGRyZXNz",
  "lamports": 5000000000
}
```

---

## Block & Slot

### slot

Display current slot information.

```bash
nusantara slot
```

**Output:**

```
Current slot:        1042
Latest stored slot:  1042
Latest root:         1040
```

**JSON output:**

```bash
nusantara --output json slot
```

```json
{
  "slot": 1042,
  "latest_stored_slot": 1042,
  "latest_root": 1040
}
```

---

### block

View block information at a specific slot.

```bash
nusantara block <SLOT>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `SLOT` | Yes | Slot number |

**Example:**

```bash
nusantara block 42
```

**Output:**

```
Slot:        42
Parent slot: 41
Block hash:  YmxvY2tfaGFzaF9iYXNlNjQ
Parent hash: cGFyZW50X2hhc2hfYmFzZTY0
Timestamp:   1700000000
Validator:   dmFsaWRhdG9yX2lkZW50aXR5
Tx count:    7
Merkle root: bWVya2xlX3Jvb3RfaGFzaA
```

---

## Transaction

### transaction

View the status and metadata of a confirmed transaction.

```bash
nusantara transaction <HASH>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `HASH` | Yes | Transaction hash / signature (Base64) |

**Example:**

```bash
nusantara transaction dHhfaGFzaF9iYXNlNjQ
```

**Output:**

```
Signature: dHhfaGFzaF9iYXNlNjQ
Slot:      42
Status:    success
Fee:       5000 lamports
CU used:   150
```

**JSON output:**

```bash
nusantara --output json transaction dHhfaGFzaF9iYXNlNjQ
```

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

---

## Epoch & Leader Schedule

### epoch-info

Display current epoch information.

```bash
nusantara epoch-info
```

**Output:**

```
Epoch:                2
Slot index:           1042/432000
Absolute slot:        865042
Timestamp:            1700000000
Leader schedule epoch: 3
Epoch progress:       0.2%
```

---

### leader-schedule

Display the leader schedule. Consecutive slots assigned to the same leader are grouped.

```bash
nusantara leader-schedule [EPOCH]
```

| Argument | Required | Description |
|----------|----------|-------------|
| `EPOCH` | No | Epoch number (defaults to current epoch) |

**Example:**

```bash
nusantara leader-schedule
```

**Output:**

```
Leader Schedule -- Epoch 2
------------------------------------------------------------
  Slots 864000-864003: dmFsaWRhdG9yXzE
  Slots 864004-864007: dmFsaWRhdG9yXzI
  Slot 864008: dmFsaWRhdG9yXzE
```

**Example (specific epoch):**

```bash
nusantara leader-schedule 3
```

---

## Validators

### validators

List all validators with their stake and voting information.

```bash
nusantara validators
```

**Output:**

```
Total active stake: 1000.0 NUSA
--------------------------------------------------------------------------------
Identity             Comm%           Stake (NUSA)  Last Vote       Root
--------------------------------------------------------------------------------
dmFsaWRhdG9yXzE...  10                     500.00       1042       1040
dmFsaWRhdG9yXzI...  5                      500.00       1041       1039
```

**JSON output:**

```bash
nusantara --output json validators
```

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
    }
  ]
}
```

---

## Stake Operations

### create-stake-account

Create and fund a new stake account.

```bash
nusantara create-stake-account <STAKE_KEYPAIR> <AMOUNT>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `STAKE_KEYPAIR` | Yes | Path to the stake account keypair file |
| `AMOUNT` | Yes | Amount in NUSA to fund the stake account |

The payer (configured keypair) becomes both the staker and withdrawer authority. The stake account keypair file must be generated beforehand with `nusantara keygen`.

**Example:**

```bash
nusantara keygen -o ~/.config/nusantara/stake.key
nusantara create-stake-account ~/.config/nusantara/stake.key 100
```

**Output:**

```
Stake account created: c3Rha2VfYWNjb3VudF9hZGRyZXNz
Signature: dHhfc2lnbmF0dXJl
```

**JSON output:**

```json
{
  "signature": "dHhfc2lnbmF0dXJl",
  "stake_account": "c3Rha2VfYWNjb3VudF9hZGRyZXNz"
}
```

---

### delegate-stake

Delegate a stake account to a vote account.

```bash
nusantara delegate-stake <STAKE_ACCOUNT> <VOTE_ACCOUNT>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `STAKE_ACCOUNT` | Yes | Stake account address (Base64) |
| `VOTE_ACCOUNT` | Yes | Vote account address to delegate to (Base64) |

Requires the configured keypair to be the authorized staker.

**Example:**

```bash
nusantara delegate-stake c3Rha2VfYWNjb3VudA dm90ZV9hY2NvdW50
```

**Output:**

```
Stake delegated to dm90ZV9hY2NvdW50
Signature: dHhfc2lnbmF0dXJl
```

---

### deactivate-stake

Deactivate a delegated stake account. The stake will be deactivated at the end of the current epoch.

```bash
nusantara deactivate-stake <STAKE_ACCOUNT>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `STAKE_ACCOUNT` | Yes | Stake account address (Base64) |

**Example:**

```bash
nusantara deactivate-stake c3Rha2VfYWNjb3VudA
```

**Output:**

```
Stake deactivated: c3Rha2VfYWNjb3VudA
Signature: dHhfc2lnbmF0dXJl
```

---

### withdraw-stake

Withdraw lamports from a deactivated stake account.

```bash
nusantara withdraw-stake <STAKE_ACCOUNT> <TO> <AMOUNT>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `STAKE_ACCOUNT` | Yes | Stake account address (Base64) |
| `TO` | Yes | Recipient address (Base64) |
| `AMOUNT` | Yes | Amount in NUSA to withdraw |

Requires the configured keypair to be the authorized withdrawer.

**Example:**

```bash
nusantara withdraw-stake c3Rha2VfYWNjb3VudA cmVjaXBpZW50 50
```

**Output:**

```
Withdrew 50 NUSA from c3Rha2VfYWNjb3VudA to cmVjaXBpZW50
Signature: dHhfc2lnbmF0dXJl
```

---

### stake-account

View detailed stake account information.

```bash
nusantara stake-account <ADDRESS>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `ADDRESS` | Yes | Stake account address (Base64) |

**Example:**

```bash
nusantara stake-account c3Rha2VfYWNjb3VudF9hZGRyZXNz
```

**Output:**

```
Stake Account: c3Rha2VfYWNjb3VudF9hZGRyZXNz
Balance:       10000000000 lamports
State:         Delegated
Staker:        c3Rha2VyX2FkZHJlc3M
Withdrawer:    d2l0aGRyYXdlcl9hZGRyZXNz
Voter:         dm90ZXJfYWRkcmVzcw
Stake:         10000000000 lamports
Activation:    epoch 2
```

---

## Vote Operations

### create-vote-account

Create a new vote account for a validator.

```bash
nusantara create-vote-account <VOTE_KEYPAIR> <IDENTITY> <COMMISSION>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `VOTE_KEYPAIR` | Yes | Path to the vote account keypair file |
| `IDENTITY` | Yes | Validator node identity address (Base64) |
| `COMMISSION` | Yes | Commission percentage (0-100) |

The payer (configured keypair) becomes both the authorized voter and authorized withdrawer.

**Example:**

```bash
nusantara keygen -o ~/.config/nusantara/vote.key
nusantara create-vote-account ~/.config/nusantara/vote.key dmFsaWRhdG9yX2lkZW50aXR5 10
```

**Output:**

```
Vote account created: dm90ZV9hY2NvdW50X2FkZHJlc3M
Signature: dHhfc2lnbmF0dXJl
```

**JSON output:**

```json
{
  "signature": "dHhfc2lnbmF0dXJl",
  "vote_account": "dm90ZV9hY2NvdW50X2FkZHJlc3M"
}
```

---

### vote-account

View detailed vote account information.

```bash
nusantara vote-account <ADDRESS>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `ADDRESS` | Yes | Vote account address (Base64) |

**Example:**

```bash
nusantara vote-account dm90ZV9hY2NvdW50X2FkZHJlc3M
```

**Output:**

```
Vote Account:     dm90ZV9hY2NvdW50X2FkZHJlc3M
Balance:          10000000 lamports
Node identity:    dmFsaWRhdG9yX2lkZW50aXR5
Voter:            dm90ZXJfYWRkcmVzcw
Withdrawer:       d2l0aGRyYXdlcl9hZGRyZXNz
Commission:       10%
Root slot:        1040
Last vote:        slot 1042
Epoch credits:
  Epoch 0: 432 credits (prev: 0)
  Epoch 1: 864 credits (prev: 432)
```

---

### vote-authorize

Change the authorized voter or withdrawer on a vote account.

```bash
nusantara vote-authorize <VOTE_ACCOUNT> <NEW_AUTH> <AUTH_TYPE>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `VOTE_ACCOUNT` | Yes | Vote account address (Base64) |
| `NEW_AUTH` | Yes | New authorized address (Base64) |
| `AUTH_TYPE` | Yes | Authorization type: `voter` or `withdrawer` |

Requires the configured keypair to be the current authority of the given type.

**Example:**

```bash
nusantara vote-authorize dm90ZV9hY2NvdW50 bmV3X3ZvdGVy voter
```

**Output:**

```
Vote voter authorized: bmV3X3ZvdGVy
Signature: dHhfc2lnbmF0dXJl
```

---

### vote-update-commission

Update the commission percentage on a vote account.

```bash
nusantara vote-update-commission <VOTE_ACCOUNT> <COMMISSION>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `VOTE_ACCOUNT` | Yes | Vote account address (Base64) |
| `COMMISSION` | Yes | New commission percentage (0-100) |

Requires the configured keypair to be the authorized withdrawer.

**Example:**

```bash
nusantara vote-update-commission dm90ZV9hY2NvdW50 5
```

**Output:**

```
Commission updated to 5%
Signature: dHhfc2lnbmF0dXJl
```

---

## Program Operations

### program-deploy

Deploy a WASM program to the blockchain. This performs a multi-step process:

1. Creates a buffer account and writes the WASM bytecode in 1 KB chunks
2. Deploys the program from the buffer

```bash
nusantara program-deploy <WASM_FILE>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `WASM_FILE` | Yes | Path to the WASM binary file |

**Example:**

```bash
nusantara program-deploy ./target/wasm32-unknown-unknown/release/my_program.wasm
```

**Output:**

```
Program deployed: cHJvZ3JhbV9hZGRyZXNz
Program data:     cHJvZ3JhbV9kYXRhX2FkZHJlc3M
Signature: dHhfc2lnbmF0dXJl
```

**JSON output:**

```json
{
  "signature": "dHhfc2lnbmF0dXJl",
  "program_address": "cHJvZ3JhbV9hZGRyZXNz",
  "program_data_address": "cHJvZ3JhbV9kYXRhX2FkZHJlc3M"
}
```

---

### program-show

Display information about a deployed program.

```bash
nusantara program-show <ADDRESS>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `ADDRESS` | Yes | Program address (Base64) |

**Example:**

```bash
nusantara program-show cHJvZ3JhbV9hZGRyZXNz
```

**Output:**

```
Program:       cHJvZ3JhbV9hZGRyZXNz
Executable:    true
Data address:  cHJvZ3JhbV9kYXRhX2FkZHJlc3M
Authority:     dXBncmFkZV9hdXRob3JpdHk
Deploy slot:   100
Bytecode size: 65536 bytes
Balance:       1000000000 lamports
```

**JSON output:**

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

---

### program-upgrade

Upgrade an existing deployed program with new WASM bytecode.

```bash
nusantara program-upgrade <PROGRAM_ADDRESS> <WASM_FILE>
```

| Argument | Required | Description |
|----------|----------|-------------|
| `PROGRAM_ADDRESS` | Yes | Address of the program to upgrade (Base64) |
| `WASM_FILE` | Yes | Path to the new WASM binary file |

Requires the configured keypair to be the program's upgrade authority.

**Example:**

```bash
nusantara program-upgrade cHJvZ3JhbV9hZGRyZXNz ./target/wasm32-unknown-unknown/release/my_program_v2.wasm
```

**Output:**

```
Program upgraded: cHJvZ3JhbV9hZGRyZXNz
Signature: dHhfc2lnbmF0dXJl
```

**JSON output:**

```json
{
  "signature": "dHhfc2lnbmF0dXJl",
  "program_address": "cHJvZ3JhbV9hZGRyZXNz"
}
```

---

## Command Summary

| Command | Description |
|---------|-------------|
| `config get` | Show current CLI configuration |
| `config set` | Update CLI configuration |
| `keygen` | Generate a new keypair |
| `balance` | Check account balance |
| `account` | View full account info |
| `transfer` | Transfer NUSA |
| `airdrop` | Request testnet airdrop |
| `slot` | Current slot info |
| `block` | View block at slot |
| `transaction` | View transaction status |
| `epoch-info` | Current epoch information |
| `leader-schedule` | View leader schedule |
| `validators` | List all validators |
| `create-stake-account` | Create and fund a stake account |
| `delegate-stake` | Delegate stake to a vote account |
| `deactivate-stake` | Deactivate a stake delegation |
| `withdraw-stake` | Withdraw from a stake account |
| `stake-account` | View stake account details |
| `create-vote-account` | Create a vote account |
| `vote-account` | View vote account details |
| `vote-authorize` | Change voter or withdrawer authority |
| `vote-update-commission` | Update commission percentage |
| `program-deploy` | Deploy a WASM program |
| `program-show` | View program information |
| `program-upgrade` | Upgrade a deployed program |

---

## Transaction Flow

All write operations (transfer, airdrop, stake, vote, program) follow the same pattern:

1. Fetch a recent blockhash from the validator (`GET /v1/blockhash`)
2. Build the instruction(s) for the operation
3. Create a `Message` with the payer as fee payer
4. Set `message.recent_blockhash` to the fetched blockhash
5. Create a `Transaction` from the message
6. Sign with all required keypairs (Dilithium3 signatures)
7. Borsh-serialize and Base64-encode the transaction
8. Submit via `POST /v1/transaction/send`

The CLI handles this entire flow automatically. The returned signature can be used with `nusantara transaction <HASH>` to check the transaction status.

---

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Error (message printed to stderr) |
