use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use nusantara_consensus::fork_choice::ForkTree;
use nusantara_crypto::hash;

fn h(s: &str) -> nusantara_crypto::Hash {
    hash(s.as_bytes())
}

fn build_linear_tree(size: u64) -> ForkTree {
    let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
    for slot in 1..=size {
        tree.add_slot(
            slot,
            slot - 1,
            hash(slot.to_le_bytes().as_ref()),
            hash((slot + 1000).to_le_bytes().as_ref()),
        )
        .unwrap();
    }
    tree
}

fn fork_tree_add_slot(c: &mut Criterion) {
    let mut group = c.benchmark_group("fork_tree_add_slot");

    for size in [100, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                build_linear_tree(size);
            });
        });
    }
    group.finish();
}

fn fork_tree_add_vote(c: &mut Criterion) {
    let mut group = c.benchmark_group("fork_tree_add_vote");

    for depth in [10, 50, 100] {
        let mut tree = build_linear_tree(depth);

        group.bench_with_input(BenchmarkId::new("depth", depth), &depth, |b, &depth| {
            b.iter(|| {
                tree.add_vote(depth, 100);
            });
        });
    }
    group.finish();
}

fn fork_tree_compute_best(c: &mut Criterion) {
    let mut group = c.benchmark_group("fork_tree_compute_best");

    for num_forks in [2, 5, 20] {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        for fork in 0..num_forks {
            let base_slot = fork * 100 + 1;
            tree.add_slot(
                base_slot,
                0,
                hash(base_slot.to_le_bytes().as_ref()),
                hash((base_slot + 1000).to_le_bytes().as_ref()),
            )
            .unwrap();
            for depth in 1..10u64 {
                tree.add_slot(
                    base_slot + depth,
                    base_slot + depth - 1,
                    hash((base_slot + depth).to_le_bytes().as_ref()),
                    hash((base_slot + depth + 1000).to_le_bytes().as_ref()),
                )
                .unwrap();
            }
            tree.add_vote(base_slot + 9, (fork + 1) * 100);
        }

        group.bench_with_input(BenchmarkId::new("forks", num_forks), &num_forks, |b, _| {
            b.iter(|| {
                tree.compute_best_fork();
            });
        });
    }
    group.finish();
}

fn fork_tree_set_root(c: &mut Criterion) {
    let mut group = c.benchmark_group("fork_tree_set_root");

    for size in [1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut tree = build_linear_tree(size);
                tree.set_root(size / 2);
            });
        });
    }
    group.finish();
}

fn fork_tree_get_ancestry(c: &mut Criterion) {
    let mut group = c.benchmark_group("fork_tree_get_ancestry");

    for depth in [10, 50, 100] {
        let tree = build_linear_tree(depth);

        group.bench_with_input(BenchmarkId::new("depth", depth), &depth, |b, &depth| {
            b.iter(|| {
                tree.get_ancestry(depth);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    fork_tree_add_slot,
    fork_tree_add_vote,
    fork_tree_compute_best,
    fork_tree_set_root,
    fork_tree_get_ancestry,
);
criterion_main!(benches);
