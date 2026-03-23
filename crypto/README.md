# nusantara-crypto

Cryptographic library for the Nusantara blockchain.

## Features

- **SHA3-512** hashing (64-byte output) for maximum collision resistance
- **Dilithium3** post-quantum digital signatures via `pqcrypto-dilithium` (CRYSTALS-Dilithium, NIST Level 3)
- **NEAR-like account IDs** with `.nusantara` suffix
- **Base64 URL-safe** encoding (no padding) for all user-facing data
- **Merkle trees** with domain separation and proof generation/verification

## Size Reference

| Type | Bytes | Base64url Chars |
|------|-------|-----------------|
| Hash | 64 | 86 |
| PublicKey | 1,952 | 2,603 |
| SecretKey | 4,032 | (never displayed) |
| Signature | 3,309 | 4,412 |

## Usage

### Hashing

```rust
use nusantara_crypto::{hash, hashv, Hasher};

let h = hash(b"hello nusantara");
println!("{}", h); // base64url encoded

let h2 = hashv(&[b"hello", b" nusantara"]);
assert_eq!(h, h2);

let mut hasher = Hasher::new();
hasher.update(b"hello");
hasher.update(b" nusantara");
assert_eq!(h, hasher.finalize());
```

### Keypair Generation, Signing, and Verification

```rust
use nusantara_crypto::Keypair;

let keypair = Keypair::generate();
let message = b"transfer 100 tokens";
let signature = keypair.sign(message);

signature.verify(keypair.public_key(), message)
    .expect("verification failed");
```

### Account IDs

```rust
use nusantara_crypto::{AccountId, Keypair};

// Named accounts (NEAR-like)
let alice = AccountId::named("alice.nusantara").unwrap();
let dex = AccountId::named("dex.alice.nusantara").unwrap();
assert!(dex.is_sub_account_of("alice.nusantara"));

// Implicit accounts (derived from public key)
let keypair = Keypair::generate();
let implicit = keypair.public_key().to_account_id();
assert!(implicit.is_implicit());
```

### Merkle Trees

```rust
use nusantara_crypto::{hash, MerkleTree};

let leaves: Vec<_> = (0..8).map(|i| hash(&[i])).collect();
let tree = MerkleTree::new(&leaves);
let proof = tree.proof(3).unwrap();
assert!(proof.verify(&leaves[3], &tree.root()));
```

## Encoding

All user-facing data uses **Base64 URL-safe encoding without padding** (RFC 4648 section 5).
This means no `+`, `/`, or `=` characters appear in encoded output.

## Dilithium3 Signatures

This crate uses CRYSTALS-Dilithium (Dilithium3, NIST Level 3) post-quantum signatures
via the `pqcrypto-dilithium` crate. Dilithium3 provides strong post-quantum security
with 1,952-byte public keys, 4,032-byte secret keys, and 3,309-byte signatures.
