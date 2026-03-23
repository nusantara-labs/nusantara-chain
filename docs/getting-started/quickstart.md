# Quick Start Guide

Build the Nusantara blockchain from source, run a single-node validator, and interact with it using the CLI.

## Prerequisites

### Rust Toolchain

Install Rust 1.93 or later:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update
rustc --version   # must be >= 1.93
```

### System Dependencies

Nusantara depends on RocksDB and cryptography libraries that require a C/C++ compiler, CMake, and pkg-config.

**macOS (Homebrew):**

```bash
brew install cmake pkg-config
```

**Ubuntu / Debian:**

```bash
sudo apt-get update
sudo apt-get install -y build-essential cmake pkg-config libclang-dev
```

**Fedora / RHEL:**

```bash
sudo dnf install -y gcc gcc-c++ cmake pkgconfig clang-devel
```

## Build from Source

```bash
git clone https://github.com/nusantara/chain.git
cd chain
cargo build --release
```

This produces two binaries:

| Binary | Path | Description |
|--------|------|-------------|
| `nusantara-validator` | `target/release/nusantara-validator` | Validator node |
| `nusantara` | `target/release/nusantara` | CLI client |

Verify the build:

```bash
./target/release/nusantara-validator --help
./target/release/nusantara --help
```

## Create a Genesis Configuration

The genesis configuration defines the initial state of the blockchain -- funded accounts, validators, and the faucet.

Create a file named `genesis.toml`:

```toml
[cluster]
name = "my-devnet"

[epoch]
slots_per_epoch = 432000

[[validators]]
identity = "generate"
vote_account = "derive"
stake_lamports = 500_000_000_000
commission = 10

[faucet]
address = "generate"
lamports = 1_000_000_000_000_000_000
```

**Fields explained:**

- `cluster.name` -- Human-readable name for the network. Used in the genesis hash derivation.
- `epoch.slots_per_epoch` -- Number of 400ms slots per epoch (432,000 = ~48 hours).
- `validators` -- Each entry creates a validator with an auto-generated identity keypair, a derived vote account, and an initial stake in lamports.
- `faucet` -- A pre-funded account for airdrops during development. 1,000,000,000 NUSA in this example (1 NUSA = 1,000,000,000 lamports).

## Run a Single-Node Validator

```bash
./target/release/nusantara-validator \
  --ledger-path ./ledger \
  --genesis-config genesis.toml \
  --enable-faucet \
  --rpc-addr 0.0.0.0:8899 \
  --metrics-addr 127.0.0.1:9090
```

The validator will:
1. Generate an identity keypair (saved to `./ledger/`)
2. Apply the genesis configuration (slot 0)
3. Start producing blocks every 400ms
4. Serve RPC on port 8899 and Prometheus metrics on port 9090

## Verify It Is Running

Check the health endpoint:

```bash
curl http://localhost:8899/v1/health | jq
```

Expected output:

```json
{
  "status": "ok"
}
```

Check the current slot:

```bash
curl http://localhost:8899/v1/slot | jq
```

### Swagger UI

Open http://localhost:8899/swagger-ui/ in a browser to explore all 16 RPC endpoints interactively.

### Prometheus Metrics

Scrape metrics at http://localhost:9090/metrics. Key gauges:

- `blocks_produced` -- Total blocks produced
- `current_slot` -- Current slot number
- `block_time_ms` -- Block production time in milliseconds
- `transactions_per_slot` -- Transactions executed per slot

## Basic CLI Usage

### Configure the CLI

Point the CLI at your running validator:

```bash
./target/release/nusantara config set --url http://localhost:8899
```

### Generate a Keypair

```bash
./target/release/nusantara keygen -o ~/.config/nusantara/id.key
```

### Airdrop Testnet Tokens

Request NUSA from the faucet (max 10 NUSA per request):

```bash
./target/release/nusantara airdrop 10
```

### Check Balance

```bash
./target/release/nusantara balance
```

### Transfer Tokens

```bash
./target/release/nusantara transfer <RECIPIENT_ADDRESS> 1.5
```

### Query Chain State

```bash
# Current slot
./target/release/nusantara slot

# Current epoch info
./target/release/nusantara epoch-info

# View a specific block
./target/release/nusantara block 0

# List validators
./target/release/nusantara validators

# Leader schedule
./target/release/nusantara leader-schedule
```

### JSON Output

All commands support `--json` for machine-readable output:

```bash
./target/release/nusantara balance --json
```

## Validator CLI Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `--ledger-path` | `ledger` | Ledger storage directory |
| `--genesis-config` | (none) | Path to `genesis.toml` |
| `--identity` | (auto-generated) | Path to validator identity keypair file |
| `--log-level` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`) |
| `--rpc-addr` | `0.0.0.0:8899` | RPC server bind address |
| `--gossip-addr` | `0.0.0.0:8000` | Gossip protocol bind address |
| `--turbine-addr` | `0.0.0.0:8001` | Turbine (block propagation) bind address |
| `--repair-addr` | `0.0.0.0:8002` | Repair service bind address |
| `--tpu-addr` | `0.0.0.0:8003` | TPU (transaction processing) bind address |
| `--tpu-forward-addr` | `0.0.0.0:8004` | TPU forward bind address |
| `--metrics-addr` | `127.0.0.1:9090` | Prometheus metrics bind address |
| `--enable-faucet` | false | Enable the airdrop endpoint |
| `--max-ledger-slots` | `256` | Slots to retain in storage (0 = retain all) |
| `--snapshot-interval` | `0` | Automatic snapshot interval in slots (0 = disabled) |
| `--leader-timeout-ms` | `800` | Timeout waiting for a block before skipping the slot |
| `--entrypoints` | (none) | Peer gossip endpoints for cluster discovery |
| `--public-host` | (none) | External hostname for Docker/Kubernetes environments |
| `--shred-version` | `1` | Network compatibility version |
| `--rpc-tls-cert` | (none) | Path to TLS certificate for HTTPS RPC |
| `--rpc-tls-key` | (none) | Path to TLS private key for HTTPS RPC |
| `--init-only` | false | Initialize genesis and exit without running |
| `--extra-validator-keys` | (none) | Comma-separated keypair paths for multi-validator genesis |
| `--generate-keypair` | (none) | Generate a keypair to the given path and exit |

## Port Summary

| Port | Protocol | Service |
|------|----------|---------|
| 8000 | UDP | Gossip peer discovery |
| 8001 | UDP | Turbine block propagation |
| 8002 | UDP | Repair service |
| 8003 | QUIC | TPU transaction ingress |
| 8004 | QUIC | TPU forward |
| 8899 | HTTP | JSON RPC API |
| 9090 | HTTP | Prometheus metrics |

## Next Steps

- [Set up a multi-validator devnet with Docker](./devnet-setup.md)
- [Write and deploy a WASM smart contract](./writing-contracts.md)
