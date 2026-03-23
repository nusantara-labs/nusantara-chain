# Writing Smart Contracts

Nusantara smart contracts are written in Rust, compiled to WASM, and executed by the validator's wasmi-based virtual machine. This guide walks through creating, building, deploying, and upgrading a counter program step by step.

## Prerequisites

- Rust 1.93+ with the WASM target installed
- A running Nusantara validator (local or devnet)

Install the WASM target:

```bash
rustup target add wasm32-unknown-unknown
```

Verify:

```bash
rustup target list --installed | grep wasm32
```

## Project Setup

Create a new library crate:

```bash
cargo new --lib my-counter
cd my-counter
```

Edit `Cargo.toml`:

```toml
[package]
name = "my-counter"
version = "0.1.0"
edition = "2024"

[dependencies]
nusantara-sdk = { path = "../chain/sdk" }
borsh = { version = "1", features = ["derive"] }

[lib]
crate-type = ["cdylib", "rlib"]
```

**Key points:**

- `crate-type = ["cdylib"]` produces a `.wasm` file suitable for deployment. The `rlib` target lets you write unit tests that run natively.
- `nusantara-sdk` is the standalone SDK that compiles to `wasm32-unknown-unknown` without pulling in the full validator stack.
- `borsh` is used for all on-chain data serialization (deterministic, compact, NEAR-compatible).

## Step 1: Define the Entrypoint

Every Nusantara program has a single entrypoint function that the VM calls for each instruction. The `entrypoint!` macro generates the WASM `extern "C"` export.

Create `src/lib.rs`:

```rust
use nusantara_sdk::prelude::*;

entrypoint!(process_instruction);

fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    msg!("Hello from my-counter!");
    Ok(())
}
```

The entrypoint function receives three arguments:

| Parameter | Type | Description |
|-----------|------|-------------|
| `program_id` | `&Pubkey` | The 64-byte address of this program |
| `accounts` | `&[AccountInfo]` | Accounts passed by the transaction |
| `data` | `&[u8]` | Raw instruction data (your program's wire format) |

It returns `ProgramResult`, which is `Result<(), ProgramError>`.

## Step 2: Define Account State

Use Borsh for deterministic serialization of on-chain state:

```rust
use borsh::{BorshSerialize, BorshDeserialize};

#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct CounterState {
    pub count: u64,
    pub authority: Pubkey,
}
```

`CounterState` will be stored in an account's data field. The `authority` records which public key is allowed to modify the counter.

## Step 3: Define Instructions

Use a simple byte tag at position `data[0]` to distinguish instructions:

```rust
/// Instruction variants for the counter program.
///
/// Wire format:
///   [0]              -- Initialize: set counter to 0, record authority
///   [1]              -- Increment by 1
///   [2, u64_le(8b)]  -- Increment by a specific value
pub enum CounterInstruction {
    Initialize,
    Increment,
    IncrementBy(u64),
}
```

## Step 4: Implement the Handler

Here is the complete `src/lib.rs`:

```rust
use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_sdk::prelude::*;

entrypoint!(process_instruction);

// -- State --

#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct CounterState {
    pub count: u64,
    pub authority: Pubkey,
}

// -- Instruction dispatch --

fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.is_empty() {
        msg!("Counter: no instruction data");
        return Err(ProgramError::InvalidInstructionData);
    }

    match data[0] {
        // Initialize: set counter to 0
        0 => {
            msg!("Counter: initialize");

            if accounts.is_empty() {
                return Err(ProgramError::NotEnoughAccountKeys);
            }
            let counter_account = &accounts[0];

            if !counter_account.is_writable {
                return Err(ProgramError::AccountNotWritable);
            }

            // In a full implementation, you would write the initial state
            // to the counter account's data via a syscall.
            msg!("Counter initialized for authority: {}", counter_account.key);
            Ok(())
        }

        // Increment by 1
        1 => {
            msg!("Counter: increment by 1");
            Ok(())
        }

        // Increment by a given value
        2 => {
            if data.len() < 9 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let value = u64::from_le_bytes(data[1..9].try_into().unwrap());
            msg!("Counter: increment by {}", value);
            Ok(())
        }

        _ => Err(ProgramError::InvalidInstructionData),
    }
}
```

## Step 5: Use the `#[program]` Macro (Alternative)

For programs with multiple instructions, the `#[program]` attribute macro generates dispatch logic automatically. Each public function becomes an instruction, identified by an 8-byte discriminator derived from the function name.

```rust
use nusantara_sdk::prelude::*;

entrypoint!(__dispatch);

#[program]
pub mod counter {
    use super::*;

    pub fn initialize(
        _program_id: &Pubkey,
        accounts: &[AccountInfo],
        _ix_data: &[u8],
    ) -> ProgramResult {
        msg!("Counter: initialize");
        if accounts.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }
        Ok(())
    }

    pub fn increment(
        _program_id: &Pubkey,
        _accounts: &[AccountInfo],
        _ix_data: &[u8],
    ) -> ProgramResult {
        msg!("Counter: increment");
        Ok(())
    }
}
```

The macro generates:
- `INITIALIZE_DISCRIMINATOR: [u8; 8]` and `INCREMENT_DISCRIMINATOR: [u8; 8]` constants
- A `__dispatch` function that reads the first 8 bytes of instruction data and routes to the matching handler

### Using the `#[derive(Accounts)]` Macro

The `Accounts` derive macro maps a slice of `AccountInfo` values to named struct fields by position:

```rust
use nusantara_sdk::prelude::*;

#[derive(Accounts)]
pub struct Initialize<'info> {
    pub payer: AccountInfo<'info>,
    pub counter: AccountInfo<'info>,
}
```

This generates a `try_from_accounts` method:

```rust
let ctx = Initialize::try_from_accounts(accounts)?;
// ctx.payer  == accounts[0]
// ctx.counter == accounts[1]
```

Returns `ProgramError::NotEnoughAccountKeys` if the accounts slice is too short.

## Build

Compile to WASM:

```bash
cargo build --target wasm32-unknown-unknown --release
```

The output is at:

```
target/wasm32-unknown-unknown/release/my_counter.wasm
```

Check the file size (must be under 512 KiB):

```bash
ls -lh target/wasm32-unknown-unknown/release/my_counter.wasm
```

### Optimization (Optional)

Reduce WASM size with `wasm-opt` (from the [binaryen](https://github.com/WebAssembly/binaryen) toolkit):

```bash
wasm-opt -Oz -o my_counter_opt.wasm \
  target/wasm32-unknown-unknown/release/my_counter.wasm
```

## Deploy

Deploy the compiled WASM to a running validator:

```bash
nusantara program-deploy target/wasm32-unknown-unknown/release/my_counter.wasm
```

Output:

```
Program deployed: <PROGRAM_ADDRESS>
Program data:     <PROGRAM_DATA_ADDRESS>
Signature: <TX_SIGNATURE>
```

The deploy process:
1. Creates a buffer account and writes the WASM bytecode in 1 KiB chunks
2. Creates the program account and its associated program-data account
3. Copies the bytecode from the buffer to the program-data account
4. The program-data account is allocated at 2x the bytecode size to allow room for upgrades

## Inspect a Deployed Program

```bash
nusantara program-show <PROGRAM_ADDRESS>
```

Output:

```
Program:       <PROGRAM_ADDRESS>
Executable:    true
Data address:  <PROGRAM_DATA_ADDRESS>
Authority:     <DEPLOYER_ADDRESS>
Deploy slot:   42
Bytecode size: 1234 bytes
Balance:       890880 lamports
```

## Upgrade

Deploy new bytecode to an existing program (only the original deployer can upgrade):

```bash
nusantara program-upgrade <PROGRAM_ADDRESS> \
  target/wasm32-unknown-unknown/release/my_counter.wasm
```

The upgrade process:
1. Creates a new buffer account with the updated bytecode
2. Swaps the program-data account's bytecode with the buffer contents
3. The program address remains the same -- clients do not need to update

## Unit Testing

Since the `entrypoint!` macro is a no-op on non-WASM targets, you can write standard Rust tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize() {
        let program_id = Pubkey::zero();
        let key = Pubkey::new([1u8; 64]);
        let owner = Pubkey::zero();
        let account = AccountInfo::new(&key, true, true, 1_000_000, &[], &owner, false);

        let result = process_instruction(&program_id, &[account], &[0]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_empty_data_returns_error() {
        let program_id = Pubkey::zero();
        let result = process_instruction(&program_id, &[], &[]);
        assert_eq!(result, Err(ProgramError::InvalidInstructionData));
    }

    #[test]
    fn test_increment_by_value() {
        let program_id = Pubkey::zero();
        let mut data = vec![2u8];
        data.extend_from_slice(&42u64.to_le_bytes());

        let result = process_instruction(&program_id, &[], &data);
        assert!(result.is_ok());
    }
}
```

Run tests natively (not under WASM):

```bash
cargo test
```

## SDK Module Reference

| Module | Description |
|--------|-------------|
| `pubkey` | 64-byte public key type (`Pubkey`), PDA derivation, `declare_id!` macro |
| `account_info` | `AccountInfo` struct with key, signer/writable flags, lamports, data, owner |
| `program_error` | `ProgramError` enum (11 built-in variants + `Custom(u32)`) and `ProgramResult` alias |
| `program` | `invoke()` and `invoke_signed()` for cross-program invocation (CPI) |
| `log` | `msg!()` macro for logging via the `nusa_log` syscall |
| `sysvar` | `Clock`, `Rent`, `EpochSchedule` sysvar accessors via syscalls |
| `entrypoint` | `entrypoint!()` macro generating the WASM `extern "C"` export |
| `syscall` | Raw `extern "C"` declarations for `nusa_*` host functions |

### Prelude

Import everything commonly needed with a single `use` statement:

```rust
use nusantara_sdk::prelude::*;
// Provides: Pubkey, AccountInfo, ProgramError, ProgramResult,
//           invoke, invoke_signed, msg!, entrypoint!, Accounts, program
```

## WASM VM Constraints

The Nusantara VM enforces strict resource limits to ensure deterministic execution:

| Constraint | Limit | Notes |
|------------|-------|-------|
| Max bytecode size | 512 KiB | Per-program `.wasm` file size |
| Max memory | 64 pages (4 MiB) | WASM linear memory |
| Max call stack depth | 256 | Nested function calls |
| Max CPI depth | 4 | Cross-program invocation nesting |
| Max return data | 1,024 bytes | Data returned to caller via CPI |
| Max log message | 10,000 bytes | Per `msg!()` invocation |
| Default compute budget | 200,000 CU | Per transaction (configurable via compute-budget program) |

### Compute Unit Costs

| Operation | Cost (CU) |
|-----------|-----------|
| Module instantiation | 10,000 |
| Memory page allocation | 1,000 per page |
| Syscall invocation (base) | 100 |
| Account data read (base) | 100 |
| Account data write (base) | 200 |
| SHA3-512 hash | 300 |
| Dilithium3 signature verify | 2,000 |
| Cross-program invocation | 1,000 |
| Log message | 100 |
| PDA derivation | 1,500 |

### Restrictions

- **No floating point**: WASM `f32`/`f64` operations are not permitted. Use fixed-point arithmetic.
- **No filesystem or network access**: Programs run in a sandboxed VM with only syscall access to chain state.
- **Deterministic execution**: All programs must produce identical results given the same inputs. No randomness, no wall-clock time.

## Program-Derived Addresses (PDAs)

PDAs allow programs to control accounts without holding a private key. They are derived deterministically from seeds and the program ID using SHA3-512:

```rust
// Derive a PDA for a user's counter
let (pda, bump) = Pubkey::find_program_address(
    &[b"counter", user_key.as_bytes()],
    program_id,
)?;
```

PDA constraints:
- Maximum 16 seeds per derivation
- Maximum 32 bytes per seed
- `find_program_address` searches for a valid bump seed (255 down to 0)

Use PDAs to:
- Create program-owned accounts with deterministic addresses
- Sign CPI calls via `invoke_signed()` using the same seeds + bump

```rust
// CPI with PDA signing
invoke_signed(
    &system_program_id,
    &[payer_account.clone(), pda_account.clone()],
    &instruction_data,
    &[&[b"counter", user_key.as_bytes(), &[bump]]],
)?;
```

## Complete Example

The repository includes a working counter example at `examples/counter/`. To build and deploy it:

```bash
cd examples/counter

# Build
cargo build --target wasm32-unknown-unknown --release

# Deploy (with a running validator)
nusantara program-deploy \
  ../../target/wasm32-unknown-unknown/release/nusantara_example_counter.wasm
```

## Serialization Guide

All on-chain data uses Borsh serialization. Key patterns:

```rust
use borsh::{BorshSerialize, BorshDeserialize};

// Simple state
#[derive(BorshSerialize, BorshDeserialize)]
pub struct TokenAccount {
    pub owner: Pubkey,       // 64 bytes (fixed)
    pub balance: u64,        // 8 bytes
    pub is_frozen: bool,     // 1 byte
}

// Enum instructions
#[derive(BorshSerialize, BorshDeserialize)]
pub enum Instruction {
    Transfer { amount: u64 },
    Approve { delegate: Pubkey, amount: u64 },
    Freeze,
}
```

Borsh guarantees:
- Deterministic byte output for identical inputs
- No schema required for deserialization (self-describing via code)
- Compact encoding (no field names, no type tags beyond enum discriminants)

## Error Handling

Return `ProgramError` variants to indicate failures. The VM reverts all account state changes on error.

Built-in error codes:

| Variant | Code | When to use |
|---------|------|-------------|
| `InvalidInstructionData` | 1 | Malformed or unrecognized instruction |
| `NotEnoughAccountKeys` | 2 | Missing required accounts |
| `MissingRequiredSignature` | 3 | An account that should be a signer is not |
| `AccountNotWritable` | 4 | An account that should be writable is read-only |
| `AccountDataTooSmall` | 5 | Account data buffer is smaller than needed |
| `InsufficientFunds` | 6 | Not enough lamports for the operation |
| `AccountAlreadyInitialized` | 7 | Re-initializing an existing account |
| `UninitializedAccount` | 8 | Using an account that has not been set up |
| `InvalidAccountData` | 9 | Account data fails validation |
| `InvalidAccountOwner` | 10 | Account is owned by a different program |
| `Custom(u32)` | N | Application-specific error codes |

## Next Steps

- [Quick start: build and run a validator](./quickstart.md)
- [Set up a multi-validator devnet with Docker](./devnet-setup.md)
- Explore the SDK source at `sdk/src/` for detailed documentation on each module
- Review the counter example at `examples/counter/` for a complete working program
