use std::str::FromStr;

use nusantara_crypto::{
    AccountId, Hash, Keypair, MerkleTree, PublicKey, Signature, Signer, hash, hashv,
};

#[test]
fn full_transaction_signing_flow() {
    let kp = Keypair::generate();
    let address = kp.address();
    let account_id = kp.account_id();

    assert_eq!(kp.public_key().to_address(), address);
    assert!(account_id.is_implicit());

    let message = b"transfer 100 tokens to alice.nusantara";
    let sig = kp.sign(message);
    sig.verify(kp.public_key(), message).unwrap();

    // AccountId round-trip
    let s = account_id.to_string();
    let parsed = AccountId::from_str(&s).unwrap();
    assert_eq!(account_id, parsed);
}

#[test]
fn signer_trait_works() {
    let kp = Keypair::generate();
    let signer: &dyn Signer = &kp;

    let msg = b"signer trait test";
    let sig = signer.sign(msg).unwrap();
    sig.verify(signer.public_key(), msg).unwrap();

    assert_eq!(signer.address(), kp.address());
    assert_eq!(signer.account_id(), kp.account_id());
}

#[test]
fn multiple_keypairs_cross_verify_fails() {
    let kp1 = Keypair::generate();
    let kp2 = Keypair::generate();

    let msg = b"same message";
    let sig1 = kp1.sign(msg);
    let sig2 = kp2.sign(msg);

    // Correct verification
    sig1.verify(kp1.public_key(), msg).unwrap();
    sig2.verify(kp2.public_key(), msg).unwrap();

    // Cross-verification must fail
    assert!(sig1.verify(kp2.public_key(), msg).is_err());
    assert!(sig2.verify(kp1.public_key(), msg).is_err());
}

#[test]
fn account_id_lifecycle() {
    // Implicit from keypair
    let kp = Keypair::generate();
    let implicit = kp.public_key().to_account_id();
    assert!(implicit.is_implicit());

    // Named account
    let alice = AccountId::named("alice.nusantara").unwrap();
    assert!(alice.is_named());
    assert!(alice.is_top_level());

    // Sub-account
    let dex = AccountId::named("dex.alice.nusantara").unwrap();
    assert!(!dex.is_top_level());
    assert!(dex.is_sub_account_of("alice.nusantara"));
    assert!(!dex.is_sub_account_of("bob.nusantara"));

    // Parent
    let parent = dex.parent().unwrap();
    assert_eq!(parent.to_string(), "alice.nusantara");
    assert!(alice.parent().is_none());

    // FromStr -> Display round-trip
    for s in ["alice.nusantara", "dex.alice.nusantara"] {
        let acc = AccountId::from_str(s).unwrap();
        assert_eq!(acc.to_string(), s);
    }

    let implicit_str = implicit.to_string();
    let parsed = AccountId::from_str(&implicit_str).unwrap();
    assert_eq!(implicit, parsed);
}

#[test]
fn merkle_proof_for_signed_transactions() {
    let n = 8;
    let keypairs: Vec<Keypair> = (0..n).map(|_| Keypair::generate()).collect();
    let messages: Vec<Vec<u8>> = (0..n).map(|i| format!("tx_{i}").into_bytes()).collect();

    let signatures: Vec<_> = keypairs
        .iter()
        .zip(messages.iter())
        .map(|(kp, msg)| kp.sign(msg))
        .collect();

    // Hash each (message, signature, pubkey) tuple
    let leaves: Vec<Hash> = (0..n)
        .map(|i| {
            hashv(&[
                &messages[i],
                signatures[i].as_bytes(),
                keypairs[i].public_key().as_bytes(),
            ])
        })
        .collect();

    let tree = MerkleTree::new(&leaves);

    // Verify proof for each leaf
    for (i, leaf) in leaves.iter().enumerate() {
        let proof = tree.proof(i).unwrap();
        assert!(
            proof.verify(leaf, &tree.root()),
            "proof failed for leaf {i}"
        );
    }

    // Tamper with one leaf
    let fake_leaf = hash(b"tampered");
    let proof = tree.proof(0).unwrap();
    assert!(!proof.verify(&fake_leaf, &tree.root()));
}

#[test]
fn borsh_roundtrip() {
    let kp = Keypair::generate();
    let msg = b"borsh test";
    let sig = kp.sign(msg);

    // Hash
    let h = hash(msg);
    let encoded = borsh::to_vec(&h).unwrap();
    let decoded: Hash = borsh::from_slice(&encoded).unwrap();
    assert_eq!(h, decoded);

    // PublicKey
    let encoded = borsh::to_vec(kp.public_key()).unwrap();
    let decoded: PublicKey = borsh::from_slice(&encoded).unwrap();
    assert_eq!(kp.public_key().clone(), decoded);

    // Signature
    let encoded = borsh::to_vec(&sig).unwrap();
    let decoded: Signature = borsh::from_slice(&encoded).unwrap();
    assert_eq!(sig, decoded);

    // Decoded signature still verifies
    decoded.verify(kp.public_key(), msg).unwrap();

    // AccountId (named)
    let acc = AccountId::named("alice.nusantara").unwrap();
    let encoded = borsh::to_vec(&acc).unwrap();
    let decoded: AccountId = borsh::from_slice(&encoded).unwrap();
    assert_eq!(acc, decoded);

    // AccountId (implicit)
    let acc = kp.public_key().to_account_id();
    let encoded = borsh::to_vec(&acc).unwrap();
    let decoded: AccountId = borsh::from_slice(&encoded).unwrap();
    assert_eq!(acc, decoded);
}

#[test]
fn empty_message_signing() {
    let kp = Keypair::generate();
    let sig = kp.sign(b"");
    sig.verify(kp.public_key(), b"").unwrap();
}

#[test]
fn large_message_signing() {
    let kp = Keypair::generate();
    let msg = vec![0xFFu8; 1_048_576]; // 1MB
    let sig = kp.sign(&msg);
    sig.verify(kp.public_key(), &msg).unwrap();
}

#[test]
fn hash_zero_is_distinct() {
    let z = Hash::zero();
    let empty = hash(b"");
    assert_ne!(z, empty);
    // zero hash is still valid
    assert_eq!(z.as_bytes(), &[0u8; 64]);
}

#[test]
fn account_id_validation_boundaries() {
    // Exactly 2-char segment (minimum)
    assert!(AccountId::named("ab.nusantara").is_ok());

    // 1-char segment (too short)
    assert!(AccountId::named("a.nusantara").is_err());

    // 63-char segment (maximum)
    let seg = "a".repeat(63);
    assert!(AccountId::named(&format!("{seg}.nusantara")).is_ok());

    // 64-char segment (too long)
    let seg = "a".repeat(64);
    assert!(AccountId::named(&format!("{seg}.nusantara")).is_err());

    // Total at 128 chars (max) with valid segments (each <= 63 chars)
    // ".nusantara" = 10 chars, so prefix can be up to 118 chars
    // Use two segments: 63 + 1 (dot) + 54 = 118
    let seg1 = "a".repeat(63);
    let seg2 = "b".repeat(54);
    let name = format!("{seg1}.{seg2}.nusantara");
    assert_eq!(name.len(), 128);
    assert!(AccountId::named(&name).is_ok());

    // Total at 129 chars (too long)
    let seg2 = "b".repeat(55);
    let name = format!("{seg1}.{seg2}.nusantara");
    assert_eq!(name.len(), 129);
    assert!(AccountId::named(&name).is_err());
}

#[test]
fn pubkey_base64_display_fromstr_roundtrip() {
    let kp = Keypair::generate();
    let pk = kp.public_key();
    let s = pk.to_string();
    let parsed: PublicKey = s.parse().unwrap();
    assert_eq!(pk.clone(), parsed);
}

#[test]
fn signature_base64_display_fromstr_roundtrip() {
    let kp = Keypair::generate();
    let sig = kp.sign(b"roundtrip");
    let s = sig.to_string();
    let parsed: Signature = s.parse().unwrap();
    assert_eq!(sig, parsed);

    // Parsed signature still verifies
    parsed.verify(kp.public_key(), b"roundtrip").unwrap();
}
