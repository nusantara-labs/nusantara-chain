# nusantara-consensus

Consensus engine for the Nusantara blockchain implementing Solana-style PoH + Tower BFT + heaviest-fork choice.

## Modules

| Module | Description |
|--------|-------------|
| `poh` | Proof of History — SHA3-512 hash chain producing ticks and slots |
| `tower` | Tower BFT — lockout-based voting with exponential backoff |
| `fork_choice` | Heaviest subtree fork selection with stake-weighted voting |
| `leader_schedule` | Deterministic stake-weighted leader rotation per epoch |
| `bank` | Consensus-focused bank with in-memory caches over RocksDB |
| `commitment` | Commitment level tracking (Processed → Confirmed → Finalized) |
| `rewards` | Partitioned epoch reward calculation and distribution |
| `replay_stage` | Block replay orchestrator wiring all components |
| `gpu` | GPU-accelerated PoH verification via wgpu + SHA3-512 WGSL shader |

## Quick Start

```rust
use nusantara_consensus::*;
use nusantara_crypto::hash;

// Create a PoH recorder
let mut recorder = PohRecorder::new(hash(b"genesis"));
let ticks = recorder.produce_slot();

// Verify the PoH chain
assert!(verify_poh_entries(&hash(b"genesis"), &ticks.iter().map(|t| t.entry.clone()).collect::<Vec<_>>()));
```

## Configuration

All consensus parameters are defined in `config.toml` and compiled into the binary at build time:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `poh.hashes_per_tick` | 12,500 | SHA3-512 iterations per tick |
| `poh.ticks_per_slot` | 64 | Ticks per slot |
| `poh.target_tick_duration_us` | 6,250 | Target tick interval (microseconds) |
| `tower.max_lockout_history` | 31 | Votes needed for root advancement |
| `tower.switch_threshold_percentage` | 38 | Minimum alternative stake for fork switch |
| `leader_schedule.num_consecutive_leader_slots` | 4 | Consecutive slots per leader |
| `commitment.supermajority_threshold` | 66 | Stake % for Confirmed status |
| `rewards.partition_count` | 4,096 | Reward distribution partitions per epoch |

## Testing

```bash
# Unit tests (60 tests)
cargo test -p nusantara-consensus --lib

# Integration tests (24 tests)
cargo test -p nusantara-consensus --tests

# Benchmarks
cargo bench -p nusantara-consensus
```

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed design and flow diagrams.
