# Storage Architecture

## System Overview

```mermaid
graph TB
    subgraph "Storage Layer"
        S[Storage]
        WB[StorageWriteBatch]
        K[Key Encoding]
    end

    subgraph "Column Families (RocksDB)"
        CF_ACC[accounts<br/>address+slot → Account]
        CF_IDX[account_index<br/>address → latest slot]
        CF_BLK[blocks<br/>slot → BlockHeader]
        CF_TX[transactions<br/>tx_hash → StatusMeta]
        CF_SIG[address_signatures<br/>addr+slot+idx → tx_hash]
        CF_SM[slot_meta<br/>slot → SlotMeta]
        CF_DS[data_shreds<br/>slot+idx → DataShred]
        CF_CS[code_shreds<br/>slot+idx → CodeShred]
        CF_BH[bank_hashes<br/>slot → Hash]
        CF_RT[roots<br/>slot → empty]
        CF_SH[slot_hashes<br/>slot → Hash]
        CF_SV[sysvars<br/>sysvar_id → Borsh]
        CF_SN[snapshots<br/>slot → Manifest]
    end

    subgraph "Consumers"
        CB[ConsensusBank]
        RS[ReplayStage]
        API[RPC / API]
    end

    S --> |read/write| CF_ACC
    S --> CF_IDX
    S --> CF_BLK
    S --> CF_TX
    S --> CF_SIG
    S --> CF_SM
    S --> CF_DS
    S --> CF_CS
    S --> CF_BH
    S --> CF_RT
    S --> CF_SH
    S --> CF_SV
    S --> CF_SN
    WB --> |atomic commit| S
    K --> |encode keys| S

    CB --> S
    RS --> S
    API --> S
```

## Key Encoding

```mermaid
graph LR
    subgraph "slot_key (8 bytes)"
        SK[slot: u64 BE]
    end

    subgraph "account_key (72 bytes)"
        AK1[address: Hash 64B]
        AK2[slot: u64 BE 8B]
    end

    subgraph "address_sig_key (76 bytes)"
        AS1[address: Hash 64B]
        AS2[slot: u64 BE 8B]
        AS3[tx_index: u32 BE 4B]
    end

    subgraph "shred_key (12 bytes)"
        SH1[slot: u64 BE 8B]
        SH2[index: u32 BE 4B]
    end
```

All keys use big-endian encoding to preserve natural ordering in RocksDB's lexicographic key space.

## Account Storage Model

```mermaid
sequenceDiagram
    participant C as Caller
    participant S as Storage
    participant ACC as accounts CF
    participant IDX as account_index CF

    Note over C,IDX: Write: put_account(address, slot, account)
    C->>S: put_account(addr, slot=5, acc)
    S->>ACC: put(addr||5, borsh(acc))
    S->>IDX: put(addr, 5)
    Note over ACC,IDX: Atomic via WriteBatch

    Note over C,IDX: Read latest: get_account(address)
    C->>S: get_account(addr)
    S->>IDX: get(addr) → slot=5
    S->>ACC: get(addr||5) → borsh bytes
    S-->>C: Account

    Note over C,IDX: Read historical: get_account_at_slot(address, slot)
    C->>S: get_account_at_slot(addr, 3)
    S->>ACC: get(addr||3) → borsh bytes
    S-->>C: Account (or None)

    Note over C,IDX: History: get_account_history(address, limit)
    C->>S: get_account_history(addr, 3)
    S->>ACC: reverse iterate from addr||MAX
    S-->>C: [(slot=5, acc), (slot=3, acc), ...]
```

## Write Batch Flow

```mermaid
graph TD
    B[StorageWriteBatch::new] --> P1[put CF_A key1 val1]
    P1 --> P2[put CF_B key2 val2]
    P2 --> D1[delete CF_A key3]
    D1 --> W[Storage::write batch]
    W --> RDB[RocksDB WriteBatch]
    RDB --> COMMIT[Atomic Commit]

    style COMMIT fill:#2d5,stroke:#333,color:#fff
```

## Prefix Iteration

```mermaid
graph TD
    subgraph "accounts CF (prefix = 64-byte address)"
        A1["addr_A || slot_1 → acc_v1"]
        A2["addr_A || slot_5 → acc_v2"]
        A3["addr_A || slot_9 → acc_v3"]
        B1["addr_B || slot_2 → acc_v1"]
        B2["addr_B || slot_7 → acc_v2"]
    end

    Q[get_account_history addr_A limit=2] -->|reverse from addr_A||MAX| A3
    A3 --> A2
    A2 -->|limit reached| STOP[Return 2 results]

    style Q fill:#fa0,stroke:#333,color:#fff
    style STOP fill:#2d5,stroke:#333,color:#fff
```

## Consensus State (bank.rs)

```mermaid
stateDiagram-v2
    [*] --> SlotProcessed: Block received
    SlotProcessed --> RootSet: set_root(slot)
    SlotProcessed --> BankHashStored: put_bank_hash(slot, hash)
    SlotProcessed --> SlotHashStored: put_slot_hash(slot, hash)

    state "Query State" as QS {
        is_root: is_root(slot) → bool
        get_latest_root: get_latest_root() → Option slot
        get_bank_hash: get_bank_hash(slot) → Option Hash
        get_slot_hash: get_slot_hash(slot) → Option Hash
    }
```

## Data Flow Summary

| Component | Input | Output | Key Format |
|-----------|-------|--------|------------|
| Account Storage | address, slot, Account | Latest or historical Account | addr(64) ++ slot(8) |
| Block Storage | BlockHeader | Header by slot, range queries | slot(8) |
| Transaction Storage | tx_hash, StatusMeta | Status by hash, sigs by address | tx_hash(64) / addr(64)+slot(8)+idx(4) |
| Shred Storage | DataShred / CodeShred | Shreds by slot+index or all per slot | slot(8) ++ index(4) |
| Slot Meta | SlotMeta | Metadata by slot | slot(8) |
| Bank State | slot, hash | Roots, bank hashes, slot hashes | slot(8) |
| Snapshots | SnapshotManifest | Manifest by slot, latest | slot(8) |
| Sysvars | Sysvar impl | Sysvar by type ID | sysvar_id(64) |
