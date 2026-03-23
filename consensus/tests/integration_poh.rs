use nusantara_consensus::gpu::GpuPohVerifier;
use nusantara_consensus::poh::{HASHES_PER_TICK, PohRecorder, TICKS_PER_SLOT, verify_poh_entries};
use nusantara_crypto::{Hash, hash};

#[test]
fn test_poh_produce_and_verify_full_slot() {
    let init = hash(b"genesis");
    let mut recorder = PohRecorder::new(init);

    let ticks = recorder.produce_slot();
    assert_eq!(ticks.len(), TICKS_PER_SLOT as usize);

    // Collect entries from ticks
    let entries: Vec<_> = ticks.iter().map(|t| t.entry.clone()).collect();
    assert!(verify_poh_entries(&init, &entries));
}

#[test]
fn test_poh_with_transaction_mixins() {
    use nusantara_consensus::poh::verify_poh_chain;

    let init = hash(b"genesis");
    let mut recorder = PohRecorder::new(init);

    // Record a transaction mixin
    let tx_hash = hash(b"tx1");
    let entry = recorder.record(&tx_hash);

    // Verify using the chain verifier that understands mixins
    // The record() does 1 hash iteration with the mixin
    let chain = vec![(1u64, Some(tx_hash), entry.hash)];
    assert!(verify_poh_chain(&init, &chain).is_ok());
}

#[test]
#[ignore] // Only runs if GPU is available
fn test_poh_gpu_matches_cpu() {
    let init = hash(b"genesis");
    let mut recorder = PohRecorder::new(init);

    // Produce entries
    let mut entries = Vec::new();
    for _ in 0..10 {
        let tick = recorder.tick();
        entries.push(tick.entry);
    }

    // CPU verification
    assert!(verify_poh_entries(&init, &entries));

    // GPU verification
    let gpu = match GpuPohVerifier::new() {
        Ok(Some(g)) => g,
        _ => {
            eprintln!("GPU not available, skipping test");
            return;
        }
    };

    let gpu_entries: Vec<(Hash, u64, Hash)> = std::iter::once(init)
        .chain(entries.iter().map(|e| e.hash))
        .zip(entries.iter())
        .map(|(prev_hash, entry)| (prev_hash, HASHES_PER_TICK, entry.hash))
        .collect();

    if !gpu_entries.is_empty() {
        let results = gpu.verify_batch(&gpu_entries).unwrap();
        assert!(results.iter().all(|&r| r));
    }
}

#[test]
fn test_poh_invalid_entry_detected() {
    let init = hash(b"genesis");
    let mut recorder = PohRecorder::new(init);

    let mut entries = Vec::new();
    for _ in 0..5 {
        let tick = recorder.tick();
        entries.push(tick.entry);
    }

    // Tamper with middle entry
    entries[2].hash = hash(b"tampered");
    assert!(!verify_poh_entries(&init, &entries));
}

#[test]
fn test_poh_cross_slot_continuity() {
    let init = hash(b"genesis");
    let mut recorder = PohRecorder::new(init);

    // Produce first slot and collect all entries
    let slot1_ticks = recorder.produce_slot();

    // Produce second slot (continues from where slot 1 ended)
    let slot2_ticks = recorder.produce_slot();

    // Both slots together should form a valid chain from genesis
    let mut all_entries = Vec::new();
    for tick in &slot1_ticks {
        all_entries.push(tick.entry.clone());
    }
    for tick in &slot2_ticks {
        all_entries.push(tick.entry.clone());
    }

    assert!(verify_poh_entries(&init, &all_entries));

    // Verify continuity: slot 2's first entry is reachable from slot 1's last hash
    let slot1_final_hash = slot1_ticks.last().unwrap().entry.hash;
    let slot2_first = &slot2_ticks[0].entry;
    let slot1_last_num = slot1_ticks.last().unwrap().entry.num_hashes;
    let delta = slot2_first.num_hashes - slot1_last_num;

    // Manually verify the chain continuation
    let mut h = slot1_final_hash;
    for _ in 0..delta {
        h = hash(h.as_bytes());
    }
    assert_eq!(h, slot2_first.hash);
}

#[test]
fn test_poh_multiple_slots_continuous() {
    let init = hash(b"genesis");
    let mut recorder = PohRecorder::new(init);

    // Produce 3 full slots worth of entries
    let mut all_entries = Vec::new();
    for _ in 0..3 {
        let ticks = recorder.produce_slot();
        for tick in ticks {
            all_entries.push(tick.entry);
        }
    }

    // All entries should form a valid chain from genesis
    assert_eq!(all_entries.len(), (TICKS_PER_SLOT * 3) as usize);
    assert!(verify_poh_entries(&init, &all_entries));
}
