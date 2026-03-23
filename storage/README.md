# nusantara-storage

Persistent storage layer for the Nusantara blockchain, built on RocksDB with 14 column families for efficient data organization.

## Modules

| Module | Description |
|--------|-------------|
| `storage` | Core `Storage` struct — RocksDB open/close, raw get/put/delete, atomic write batches |
| `cf` | Column family definitions, prefix extractors, and descriptors |
| `keys` | Fixed-width binary key encoding (slot, account, address-signature, shred) |
| `write_batch` | `StorageWriteBatch` — staged multi-CF atomic operations |
| `account_index` | Account storage with versioning (latest pointer + historical snapshots) |
| `block` | Block header storage with slot-range queries |
| `transaction` | Transaction status metadata and address-to-signature mappings |
| `slot_meta` | Slot metadata (shred counts, connectivity, completion status) |
| `shred` | Data and code (erasure) shred storage with per-slot iteration |
| `bank` | Consensus state — roots, bank hashes, slot hashes |
| `snapshot` | Snapshot manifests for checkpoint/restore |
| `sysvar` | Generic sysvar storage keyed by type ID |
| `error` | `StorageError` enum (RocksDB, serialization, corruption) |

## Quick Start

```rust
use nusantara_storage::Storage;
use nusantara_core::Account;
use nusantara_crypto::hash;

let storage = Storage::open(std::path::Path::new("/tmp/nusantara-db")).unwrap();

// Store and retrieve an account
let address = hash(b"alice");
let account = Account::new(1_000_000_000, hash(b"system"));
storage.put_account(&address, 1, &account).unwrap();
let loaded = storage.get_account(&address).unwrap();
```

## Column Families

| CF Name | Key Format | Value | Prefix Extractor |
|---------|------------|-------|------------------|
| `default` | arbitrary | arbitrary | none |
| `accounts` | address(64) ++ slot(8 BE) | Borsh `Account` | address (64 bytes) |
| `account_index` | address(64) | slot(8 BE) | none |
| `blocks` | slot(8 BE) | Borsh `BlockHeader` | none |
| `transactions` | tx_hash(64) | Borsh `TransactionStatusMeta` | none |
| `address_signatures` | address(64) ++ slot(8 BE) ++ tx_index(4 BE) | tx_hash(64) | address (64 bytes) |
| `slot_meta` | slot(8 BE) | Borsh `SlotMeta` | none |
| `data_shreds` | slot(8 BE) ++ index(4 BE) | Borsh `DataShred` | slot (8 bytes) |
| `code_shreds` | slot(8 BE) ++ index(4 BE) | Borsh `CodeShred` | slot (8 bytes) |
| `bank_hashes` | slot(8 BE) | Hash(64) | none |
| `roots` | slot(8 BE) | empty | none |
| `slot_hashes` | slot(8 BE) | Hash(64) | none |
| `sysvars` | sysvar_id(64) | Borsh sysvar | none |
| `snapshots` | slot(8 BE) | Borsh `SnapshotManifest` | none |

## Key Design Decisions

- **Fixed-width binary keys** — No Borsh length-prefixes in keys; raw big-endian bytes for correct lexicographic ordering
- **Prefix extractors** — RocksDB prefix bloom filters on accounts (by address) and shreds (by slot) for fast prefix iteration
- **Account versioning** — `accounts` CF stores every version (address+slot), `account_index` CF points to the latest slot for O(1) lookup
- **Atomic batches** — `StorageWriteBatch` groups cross-CF writes into a single RocksDB `WriteBatch` for atomicity
- **Borsh serialization** — All structured values use Borsh for deterministic, compact encoding

## Testing

```bash
# Unit tests (53 tests)
cargo test -p nusantara-storage --lib

# Integration tests
cargo test -p nusantara-storage --tests

# Benchmarks
cargo bench -p nusantara-storage
```

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed design and data flow diagrams.
