# Consensus Architecture

## System Overview

```mermaid
graph TB
    subgraph "Consensus Engine"
        RS[ReplayStage]
        POH[PohRecorder]
        TW[Tower BFT]
        FC[ForkTree]
        CT[CommitmentTracker]
        LS[LeaderSchedule]
        BK[ConsensusBank]
        RW[RewardsCalculator]
        GPU[GpuPohVerifier]
    end

    subgraph "Storage Layer"
        DB[(RocksDB)]
    end

    subgraph "External"
        NET[Network Layer]
        TX[Transactions]
    end

    NET -->|blocks| RS
    TX -->|vote txs| RS
    RS --> POH
    RS --> TW
    RS --> FC
    RS --> CT
    RS --> LS
    RS --> BK
    RS --> GPU
    BK --> DB
    BK --> RW
```

## Module Interactions

### Block Replay Flow

```mermaid
sequenceDiagram
    participant Net as Network
    participant RS as ReplayStage
    participant POH as PoH Verifier
    participant FC as ForkTree
    participant TW as Tower BFT
    participant CT as Commitment
    participant BK as Bank
    participant DB as Storage

    Net->>RS: Block + PoH entries
    RS->>RS: Verify leader (LeaderSchedule)
    RS->>POH: Verify PoH chain (GPU/CPU)
    POH-->>RS: Valid/Invalid
    RS->>FC: add_slot(slot, parent, hashes)
    RS->>TW: process_vote(vote) for each vote tx
    TW-->>RS: TowerVoteResult {new_root, lockouts}
    RS->>FC: add_vote(slot, stake) for each vote
    RS->>CT: record_vote(slot, stake)
    CT-->>RS: CommitmentLevel
    alt Root Advanced
        RS->>FC: set_root(new_root) -> pruned slots
        RS->>CT: mark_finalized(root)
        RS->>DB: set_root(slot)
    end
    RS->>BK: freeze(slot) -> FrozenBankState
    RS->>DB: put_bank_hash, put_slot_hash
```

### Tower BFT Voting

```mermaid
graph TD
    V[New Vote at Slot S] --> CHK{Check Lockouts}
    CHK -->|Locked Out| ERR[LockoutViolation Error]
    CHK -->|OK| EXP[Expire Old Lockouts]
    EXP --> PUSH[Push New Lockout<br/>confirmation_count=1]
    PUSH --> INC[Increment All<br/>confirmation_counts]
    INC --> ROOT{Bottom Vote<br/>count >= 31?}
    ROOT -->|Yes| ADV[Advance Root]
    ROOT -->|No| DONE[Done]
    ADV --> DONE
```

Each lockout has a slot and confirmation_count. The lockout duration is `2^confirmation_count` slots. A vote at slot S is locked out until `slot + 2^confirmation_count`. After 31 confirmations, the vote becomes a finalized root.

### Fork Choice Algorithm

```mermaid
graph TD
    R[Root Slot 0] --> A[Slot 1<br/>subtree: 300]
    R --> B[Slot 4<br/>subtree: 150]
    A --> C[Slot 2<br/>subtree: 300]
    A --> D[Slot 3<br/>subtree: 0]
    C --> E[Slot 5<br/>stake: 300]
    B --> F[Slot 6<br/>stake: 150]

    style E fill:#2d5,stroke:#333,color:#fff
    style F fill:#d52,stroke:#333,color:#fff
```

The heaviest subtree fork choice walks the tree from root, always choosing the child with the highest cumulative subtree stake. In the example above, the best fork is `0 -> 1 -> 2 -> 5` with 300 total stake.

### Proof of History Chain

```mermaid
graph LR
    G[Genesis Hash] -->|12500 hashes| T1[Tick 1]
    T1 -->|12500 hashes| T2[Tick 2]
    T2 -->|mixed TX| TX1[TX Entry]
    TX1 -->|remaining hashes| T3[Tick 3]
    T3 -->|...| T64[Tick 64<br/>= 1 Slot]

    style TX1 fill:#fa0,stroke:#333,color:#fff
```

- **Pure hash**: `hash = SHA3-512(hash)` repeated N times
- **Transaction mixin**: `hash = SHA3-512(hash || tx_hash)` — proves TX existed at this point
- **Tick**: After `HASHES_PER_TICK` (12,500) iterations
- **Slot**: After `TICKS_PER_SLOT` (64) ticks = 800,000 total hashes

### Commitment Levels

```mermaid
stateDiagram-v2
    [*] --> Processed: Block received
    Processed --> Confirmed: >= 66% stake voted
    Confirmed --> Finalized: Tower root advanced past slot
```

- **Processed**: Block has been received and validated
- **Confirmed**: Supermajority (66%) of stake has voted for this slot
- **Finalized**: Tower root has advanced past this slot (irreversible)

### Epoch Boundary & Rewards

```mermaid
graph TD
    EB[Epoch Boundary] --> RS[Recalculate Stakes]
    EB --> LS[Compute Leader Schedule]
    EB --> RC[Calculate Rewards]
    RS --> WU{Warmup/Cooldown}
    WU -->|Activating| WR[Apply 25% warmup rate]
    WU -->|Active| FR[Full stake]
    WU -->|Deactivating| CR[Apply cooldown rate]
    RC --> PT[Calculate Points<br/>credits x stake]
    PT --> PV[Point Value =<br/>inflation / total_points]
    PV --> PART[Partition into 4096 groups<br/>by hash of stake_account]
    PART --> DIST[Distribute over 4096 slots]
```

### Leader Schedule Generation

```mermaid
graph TD
    SEED[Epoch Seed + Epoch Number] --> RNG[Deterministic PRNG]
    STAKES[Validator Stakes] --> CDF[Cumulative Distribution]
    RNG --> SAMPLE[Stake-Weighted Sampling]
    CDF --> SAMPLE
    SAMPLE --> ASSIGN[Assign 4 Consecutive Slots]
    ASSIGN --> REPEAT{More Slots?}
    REPEAT -->|Yes| SAMPLE
    REPEAT -->|No| SCHEDULE[Leader Schedule<br/>432,000 slots]
```

## Data Flow Summary

| Component | Input | Output | Persistence |
|-----------|-------|--------|-------------|
| PohRecorder | Previous hash | Ticks, entries | None (in-memory) |
| Tower | Vote | Root advancement, lockouts | VoteState (Borsh) |
| ForkTree | Slot, parent, votes | Best fork, pruned slots | None (in-memory) |
| LeaderSchedule | Stakes, epoch seed | Slot-to-leader mapping | None (cached) |
| ConsensusBank | Storage, epoch schedule | Vote/stake caches | RocksDB |
| CommitmentTracker | Votes, stake | Commitment levels | None (in-memory) |
| RewardsCalculator | Vote states, delegations | Partitioned rewards | None (computed) |
| GpuPohVerifier | PoH entries | Verification results | None |
| ReplayStage | Blocks | Replay results | RocksDB (via Bank) |
