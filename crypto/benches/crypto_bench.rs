use criterion::{Criterion, black_box, criterion_group, criterion_main};
use nusantara_crypto::{Hash, Hasher, Keypair, MerkleTree, hash, hashv};

fn bench_hashing(c: &mut Criterion) {
    let mut group = c.benchmark_group("hashing");

    for size in [32, 256, 1024, 1_048_576] {
        let data = vec![0xABu8; size];
        group.bench_function(format!("hash_{size}B"), |b| {
            b.iter(|| hash(black_box(&data)))
        });
    }

    group.bench_function("hashv_3_slices", |b| {
        let s1 = [1u8; 100];
        let s2 = [2u8; 200];
        let s3 = [3u8; 300];
        b.iter(|| hashv(black_box(&[&s1[..], &s2[..], &s3[..]])))
    });

    group.bench_function("hasher_incremental_3x100B", |b| {
        let chunks = [[0u8; 100]; 3];
        b.iter(|| {
            let mut h = Hasher::new();
            for chunk in &chunks {
                h.update(black_box(chunk));
            }
            h.finalize()
        })
    });

    group.finish();
}

fn bench_keypair(c: &mut Criterion) {
    c.bench_function("keypair_generate", |b| b.iter(Keypair::generate));
}

fn bench_signing(c: &mut Criterion) {
    let mut group = c.benchmark_group("signing");
    let kp = Keypair::generate();

    for size in [32, 256, 1024] {
        let msg = vec![0xCDu8; size];
        group.bench_function(format!("sign_{size}B"), |b| {
            b.iter(|| kp.sign(black_box(&msg)))
        });
    }

    group.finish();
}

fn bench_verification(c: &mut Criterion) {
    let mut group = c.benchmark_group("verification");
    let kp = Keypair::generate();

    for size in [32, 256, 1024] {
        let msg = vec![0xCDu8; size];
        let sig = kp.sign(&msg);
        group.bench_function(format!("verify_{size}B"), |b| {
            b.iter(|| sig.verify(black_box(kp.public_key()), black_box(&msg)))
        });
    }

    group.finish();
}

fn bench_merkle(c: &mut Criterion) {
    let mut group = c.benchmark_group("merkle");

    for count in [10, 100, 1000, 10000] {
        let leaves: Vec<Hash> = (0..count)
            .map(|i| hash(&(i as u64).to_le_bytes()))
            .collect();

        group.bench_function(format!("new_{count}_leaves"), |b| {
            b.iter(|| MerkleTree::new(black_box(&leaves)))
        });

        let tree = MerkleTree::new(&leaves);

        group.bench_function(format!("proof_{count}_leaves"), |b| {
            b.iter(|| tree.proof(black_box(0)))
        });

        let proof = tree.proof(0).unwrap();
        let root = tree.root();

        group.bench_function(format!("verify_{count}_leaves"), |b| {
            b.iter(|| proof.verify(black_box(&leaves[0]), black_box(&root)))
        });
    }

    group.finish();
}

fn bench_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("encoding");

    let h = hash(b"benchmark data");
    group.bench_function("hash_to_base64", |b| b.iter(|| black_box(&h).to_base64()));
    let encoded = h.to_base64();
    group.bench_function("hash_from_base64", |b| {
        b.iter(|| Hash::from_base64(black_box(&encoded)))
    });

    let kp = Keypair::generate();
    let pk = kp.public_key().clone();
    group.bench_function("pubkey_to_base64", |b| {
        b.iter(|| black_box(&pk).to_base64())
    });
    let pk_encoded = pk.to_base64();
    group.bench_function("pubkey_from_base64", |b| {
        b.iter(|| nusantara_crypto::PublicKey::from_base64(black_box(&pk_encoded)))
    });

    group.finish();
}

fn bench_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialization");

    let h = hash(b"borsh bench");
    group.bench_function("hash_borsh_serialize", |b| {
        b.iter(|| borsh::to_vec(black_box(&h)))
    });
    let encoded = borsh::to_vec(&h).unwrap();
    group.bench_function("hash_borsh_deserialize", |b| {
        b.iter(|| borsh::from_slice::<Hash>(black_box(&encoded)))
    });

    let acc = nusantara_crypto::AccountId::named("alice.nusantara").unwrap();
    group.bench_function("account_id_borsh_serialize", |b| {
        b.iter(|| borsh::to_vec(black_box(&acc)))
    });
    let acc_encoded = borsh::to_vec(&acc).unwrap();
    group.bench_function("account_id_borsh_deserialize", |b| {
        b.iter(|| borsh::from_slice::<nusantara_crypto::AccountId>(black_box(&acc_encoded)))
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_hashing,
    bench_keypair,
    bench_signing,
    bench_verification,
    bench_merkle,
    bench_encoding,
    bench_serialization,
);
criterion_main!(benches);
