# Validator Operations

This document covers the validator lifecycle, operational procedures, and common
administrative tasks for running a Nusantara validator.

## Boot Sequence

When `nusantara-validator` starts, it executes the following steps in order:

1. **Open storage** -- Open or create a RocksDB database at the `--ledger-path` directory.
2. **Load keypair** -- Read the Dilithium3 identity keypair from disk (5,984 raw bytes:
   1,952-byte public key + 4,032-byte secret key). If `--generate-keypair` is specified,
   generate a new keypair and write it to the given path.
3. **Parse genesis** -- Read and parse `genesis.toml` from the path specified by
   `--genesis-config`.
4. **Apply genesis** -- Build the genesis state (slot 0) from the parsed config.
   This step is idempotent: the presence of `GENESIS_HASH_KEY` in `CF_DEFAULT` prevents
   re-initialization. Genesis creates funded accounts, validator vote and stake accounts,
   sysvars, and the genesis block.
5. **Load sysvars** -- Read sysvar state (Clock, Rent, EpochSchedule, SlotHashes,
   StakeHistory, etc.) from storage into the SysvarCache.
6. **Create ConsensusBank** -- Initialize the consensus bank with loaded state, including
   the stake-weighted validator set and current epoch parameters.
7. **Register validators** -- Deserialize `Vec<GenesisValidatorInfo>` from `VALIDATORS_KEY`
   in `CF_DEFAULT` and register each validator in the consensus bank.
8. **Create SlotClock** -- Initialize the wall-clock slot timer using the genesis creation
   time and the 400ms slot duration.
9. **Create BlockProducer** -- Initialize the block producer with a Proof-of-History chain
   seeded from the latest block hash.
10. **Start networking** -- Launch GossipService (UDP), TurbineReceiver (UDP), and
    TpuService (QUIC) as background tokio tasks.
11. **Start RPC server** -- Bind the Axum HTTP server with REST endpoints and Swagger UI.
12. **Start metrics exporter** -- Bind the Prometheus metrics HTTP endpoint.
13. **Enter main loop** -- Begin slot-driven block production and consensus.

## Main Loop

The main loop is driven by the SlotClock, which maps wall-clock time to slot numbers.

```
loop {
    slot = slot_clock.wait_for_next_slot().await

    if leader_schedule.is_leader(slot, my_identity):
        transactions = drain_mempool()
        block = block_producer.produce_block(slot, transactions)
        broadcast_stage.broadcast(block)
        publish_event(SlotUpdate, BlockNotification)
    else:
        match turbine_receiver.wait_for_block(slot, leader_timeout_ms):
            Ok(block) => replay_stage.replay(block)
            Timeout   => record_skip(slot)

    tower.process_vote(slot)
    fork_choice.update()

    if epoch_boundary(slot):
        compute_leader_schedule(next_epoch)
        process_rewards()
}
```

- **Leader timeout**: If the expected leader does not produce a block within
  `leader_timeout_ms` (default 800ms), the slot is skipped.
- **Epoch boundary**: At the first slot of each epoch (every 432,000 slots), the leader
  schedule for the next epoch is computed from stake weights, and staking rewards are
  distributed.
- **PubsubEvents**: `SlotUpdate` and `BlockNotification` events are published to
  WebSocket subscribers on each slot transition.

## Graceful Shutdown

The validator handles `SIGINT` (Ctrl+C) through a `tokio::select!` shutdown path:

1. Signal received by the main loop
2. RPC server stops accepting new connections and drains in-flight requests
3. Networking tasks (gossip, turbine, TPU) are cancelled
4. BlockProducer finishes any in-progress block (or aborts)
5. Storage is flushed and RocksDB WAL is synced
6. Process exits with code 0

## Snapshots

Snapshots allow new validators to bootstrap without replaying the full ledger history.

- **Enable**: `--snapshot-interval=N` creates a snapshot every N slots (default 0 = disabled)
- **Storage**: `SnapshotManifest` is written to `CF_SNAPSHOTS` in RocksDB
- **Download via REST**:
  - `GET /v1/snapshot/latest` -- returns snapshot metadata (slot, hash, size)
  - `GET /v1/snapshot/download` -- streams the binary snapshot
- **Bootstrap**: A new validator can download a snapshot and start from that state instead
  of replaying from genesis. The validator verifies the snapshot hash before applying.

## Ledger Pruning

The `--max-ledger-slots` argument controls how many historical slots are retained.

| Setting | Behavior |
|---------|----------|
| `--max-ledger-slots=256` (default) | Keep the last 256 slots from the current root |
| `--max-ledger-slots=0` | Keep all history (archive node) |
| `--max-ledger-slots=1000` | Keep the last 1000 slots |

**What is pruned:**
- Blocks and block headers
- Transaction data and status metadata
- Data shreds and code shreds
- Slot metadata and bank hashes

**What is NOT pruned:**
- Account state in the accounts column family (separate lifecycle)
- Snapshot manifests
- Genesis marker

Pruning runs asynchronously after each new root is set. It does not block block production.

## Keypair Management

### Generating a New Keypair

```bash
nusantara-validator --generate-keypair /path/to/identity.key
```

This writes 5,984 raw bytes: the 1,952-byte Dilithium3 public key followed by the
4,032-byte secret key.

### Keypair Format

| Segment | Offset | Size | Content |
|---------|--------|------|---------|
| Public key | 0 | 1,952 bytes | Dilithium3 public key |
| Secret key | 1,952 | 4,032 bytes | Dilithium3 secret key |
| Total | 0 | 5,984 bytes | Raw binary, no encoding |

### Identity Derivation

The validator's identity (address) is derived by computing the SHA3-512 hash of the public
key. This identity is used throughout the system: gossip advertisements, leader schedule,
vote accounts, and faucet transfers (if enabled).

### Security

- Store the secret key with restrictive file permissions (`chmod 600`)
- The secret key controls the validator's identity and, if `--enable-faucet` is set, the
  ability to mint tokens
- Back up the keypair -- losing it means losing the validator identity
- In Docker deployments, keypairs are generated by the `genesis-init` service and shared
  via the `genesis-data` volume

## Common Operations

### Health Check

```bash
curl http://localhost:8899/v1/health | jq
```

Returns the validator's health status including whether it is synced and producing blocks.

### Current Slot

```bash
curl http://localhost:8899/v1/slot | jq
```

Returns the current slot number the validator is processing.

### Validator Set

```bash
curl http://localhost:8899/v1/validators | jq
```

Lists all known validators with their identity, stake, and last vote slot.

### Epoch Info

```bash
curl http://localhost:8899/v1/epoch-info | jq
```

Returns current epoch number, slot index within the epoch, slots remaining, and epoch
duration.

### Block Details

```bash
curl http://localhost:8899/v1/block/{slot} | jq
```

Returns the block at the given slot, including header, transactions, and metadata.

### Account Lookup

```bash
curl http://localhost:8899/v1/account/{address} | jq
```

Returns the account state including balance, owner program, and data.

### Raw Prometheus Metrics

```bash
curl -s http://localhost:9090/metrics
```

Returns all metrics in Prometheus exposition format.

### Swagger UI

Open `http://localhost:8899/swagger-ui/` in a browser to explore the full RPC API with
interactive documentation.

## Troubleshooting

### Validator Not Producing Blocks

1. Check health: `curl http://localhost:8899/v1/health | jq`
2. Check slot progress: `curl http://localhost:8899/v1/slot | jq`
3. Verify the validator is in the leader schedule for the current epoch
4. Check logs for errors: `RUST_LOG=debug` for verbose output
5. Verify gossip connectivity: check `nusantara_gossip_push_messages` metric is non-zero

### Validator Falling Behind

1. Check `nusantara_block_time_ms` histogram -- if p99 exceeds 400ms, the validator
   cannot keep up with slot time
2. Check system resources: CPU, memory, disk I/O
3. Check network latency to peer validators
4. Consider increasing `--max-ledger-slots` if disk I/O from pruning is a bottleneck

### Storage Issues

1. Check available disk space
2. RocksDB may need compaction -- this happens automatically but can be triggered by
   restarting the validator
3. If the database is corrupted, restore from a snapshot or replay from genesis

### Networking Issues

1. Verify UDP ports 8000-8002 are open between validators
2. Verify QUIC ports 8003-8004 are reachable
3. Check that `--entrypoints` are correctly configured
4. Check firewall rules and Docker network configuration
