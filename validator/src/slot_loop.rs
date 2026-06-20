use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use nusantara_core::DEFAULT_SLOT_DURATION_MS;
use nusantara_core::block::Block;
use nusantara_crypto::Hash;
use nusantara_rpc::PubsubEvent;
use nusantara_storage::StorageWriteBatch;
use nusantara_storage::cf::{CF_BANK_HASHES, CF_BLOCKS, CF_DEFAULT, CF_SLOT_HASHES, CF_SLOT_META};
use nusantara_storage::keys::slot_key;
use nusantara_sysvar_program::SlotHashes;
use nusantara_turbine::turbine_tree::TURBINE_FANOUT;
use nusantara_turbine::{BroadcastStage, TurbineTree};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::block_producer::PendingBlockStorage;
use crate::cli::Cli;
use crate::constants::{CATCHUP_THRESHOLD, GOSSIP_REPORT_INTERVAL, LEDGER_PRUNE_INTERVAL};
use crate::error::ValidatorError;
use crate::helpers;
use crate::node::ValidatorNode;

impl ValidatorNode {
    #[tracing::instrument(skip_all, fields(start_slot = self.current_slot))]
    pub async fn run(&mut self, cli: &Cli) -> Result<(), ValidatorError> {
        info!(start_slot = self.current_slot, "starting validator");

        let services = self.spawn_services(cli).await?;
        let mut block_rx = services.block_rx;
        let broadcast_stage = services.broadcast_stage;
        let current_slot_shared = services.current_slot_shared;
        let shutdown_tx = services.shutdown_tx;
        let mut service_tasks = services.service_tasks;

        loop {
            tokio::select! {
                biased;
                _ = tokio::signal::ctrl_c() => {
                    info!("received shutdown signal");
                    let _ = shutdown_tx.send(true);
                    break;
                }
                Some(result) = service_tasks.join_next() => {
                    match result {
                        Ok(name) => {
                            // If shutdown was already requested (ctrl_c path), services
                            // exit cleanly — log at info, not error, to avoid false alarm.
                            if *shutdown_tx.borrow() {
                                tracing::info!(service = name, "service stopped");
                            } else {
                                tracing::error!(service = name, "service exited unexpectedly");
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "service task panicked");
                        }
                    }
                    let _ = shutdown_tx.send(true);
                    break;
                }
                _ = self.slot_clock.wait_for_slot(self.current_slot) => {
                    // Update shared current_slot for TPU closure and metrics
                    current_slot_shared.store(self.current_slot, Ordering::Relaxed);
                    metrics::gauge!("nusantara_current_slot").set(self.current_slot as f64);

                    // Track replay gap — how far behind wall-clock the validator is
                    let replay_tip = self.replay_stage.current_tip();
                    let replay_gap = self.current_slot.saturating_sub(replay_tip);
                    metrics::gauge!("nusantara_replay_gap").set(replay_gap as f64);
                    metrics::gauge!("nusantara_replay_tip").set(replay_tip as f64);
                    if replay_gap > 128 {
                        tracing::warn!(
                            current_slot = self.current_slot,
                            replay_tip,
                            replay_gap,
                            orphan_count = self.orphan_blocks.len(),
                            "replay gap exceeds 128 slots — validator falling behind"
                        );
                    }

                    let catching_up = replay_gap > CATCHUP_THRESHOLD;
                    if catching_up {
                        metrics::counter!("nusantara_catchup_mode_entered").increment(1);
                    }

                    if self.am_i_leader(self.current_slot) {
                        self.leader_slot(&broadcast_stage, &mut block_rx).await?;
                    } else {
                        // During catch-up, use zero timeout to drain blocks
                        // without waiting — the slot loop runs at full speed
                        // for past slots and we must not block on each one.
                        let effective_timeout = if catching_up { 0 } else { cli.leader_timeout_ms };
                        self.non_leader_slot(&mut block_rx, effective_timeout, catching_up).await?;
                    }

                    self.process_gossip_votes();

                    // Check for fork switch (F3) with dedup to prevent spam
                    if let Some(plan) = self.replay_stage.check_fork_switch() {
                        let target = plan.replay_slots.last().copied()
                            .unwrap_or(plan.common_ancestor);
                        if self.failed_fork_targets.contains(&target) {
                            tracing::trace!(target, "skipping fork switch — already failed");
                        } else if self.last_fork_switch_target == Some(target) {
                            tracing::trace!(target, "skipping fork switch — same target as last attempt");
                        } else {
                            self.last_fork_switch_target = Some(target);
                            self.handle_fork_switch(plan);
                        }
                    }

                    // When catching up, replay blocks from local storage
                    // first (much faster than network repair). Loop until
                    // no more progress so all available blocks are replayed
                    // in a single slot tick rather than one batch per tick.
                    if catching_up {
                        loop {
                            let n = self.catch_up_from_local_storage()?;
                            if n == 0 {
                                break;
                            }
                        }
                    }

                    // Proactive root advancement: every 32 slots, if the
                    // fork tree has grown past 48 nodes, advance root to
                    // the ancestry midpoint. This prevents tree exhaustion
                    // without waiting for the reactive 128-node limit.
                    {
                        let node_count = self.replay_stage.fork_tree().node_count();
                        let soft_limit = if catching_up { 48 } else { 128 };
                        let should_advance = if catching_up {
                            // During catch-up, check every slot
                            node_count > soft_limit
                        } else {
                            // Steady state: check every 32 slots
                            self.current_slot.is_multiple_of(32) && node_count > soft_limit
                        };
                        if should_advance {
                            let best = self.replay_stage.fork_tree().best_slot();
                            let ancestry = self.replay_stage.fork_tree().get_ancestry(best);
                            if ancestry.len() > 4 {
                                let proposed = ancestry[ancestry.len() / 2];
                                tracing::info!(
                                    proposed,
                                    node_count,
                                    best,
                                    catching_up,
                                    "proactive root advancement"
                                );
                                self.try_advance_root(proposed, catching_up)?;
                                metrics::counter!("nusantara_proactive_root_advances").increment(1);
                            }
                        }
                    }

                    // Proactive repair: when catching up, request sequential
                    // slots even if no orphans have triggered repair yet.
                    // This bootstraps the repair pipeline for fresh containers.
                    if catching_up {
                        self.request_missing_slots();
                    }

                    self.submit_vote(self.current_slot);
                    self.process_orphan_queue()?;
                    self.check_epoch_boundary(cli.snapshot_interval).await;

                    // Periodically expire mempool transactions with stale blockhashes.
                    // Use the bank's slot_hashes (up to 512 entries, covering ~200s at
                    // 400ms slots) rather than the fork tree ancestry, which is
                    // aggressively pruned by set_root and may contain only 8-12 entries.
                    if self.current_slot.is_multiple_of(10) {
                        let slot_hashes = self.bank.slot_hashes();
                        let valid_blockhashes: HashSet<Hash> =
                            slot_hashes.0.iter().map(|(_, h)| *h).collect();
                        self.mempool.remove_expired(&valid_blockhashes);
                    }

                    // Periodically report gossip peer count
                    if self.current_slot.is_multiple_of(GOSSIP_REPORT_INTERVAL) {
                        let peer_count = self.cluster_info.peer_count();
                        metrics::gauge!("nusantara_gossip_peers").set(peer_count as f64);
                    }

                    // Periodic ledger pruning (offloaded to blocking thread)
                    if cli.max_ledger_slots > 0
                        && self.current_slot.is_multiple_of(LEDGER_PRUNE_INTERVAL)
                    {
                        let min_slot =
                            self.current_slot.saturating_sub(cli.max_ledger_slots);
                        if min_slot > 0 {
                            let storage = self.storage.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Err(e) = storage.purge_slots_below(min_slot) {
                                    tracing::warn!(error = %e, min_slot, "ledger pruning failed");
                                }
                            });
                        }
                    }

                    self.current_slot += 1;
                }
            }
        }

        info!("validator shutdown complete");
        Ok(())
    }

    pub(crate) fn am_i_leader(&self, slot: u64) -> bool {
        let epoch = self.epoch_schedule.get_epoch(slot);
        let mut cache = self.leader_cache.lock();

        // Populate cache on miss — one lock acquisition covers both the check
        // and the potential insert (LRU requires &mut for reads too).
        if !cache.contains(&epoch) {
            let stakes = self.bank.get_stake_distribution();
            if let Ok(schedule) = self
                .leader_schedule_generator
                .compute_schedule(epoch, &stakes, &self.genesis_hash)
            {
                cache.put(epoch, schedule);
            }
        }

        cache
            .get(&epoch)
            .and_then(|s| s.get_leader(slot, &self.epoch_schedule))
            .map(|leader| *leader == self.identity)
            .unwrap_or(false)
    }

    #[tracing::instrument(skip_all, fields(slot = self.current_slot))]
    async fn leader_slot(
        &mut self,
        broadcast: &BroadcastStage,
        block_rx: &mut mpsc::Receiver<Block>,
    ) -> Result<(), ValidatorError> {
        // 1. Catch up on pending blocks from previous leader
        let mut pending = Vec::new();
        while let Ok(block) = block_rx.try_recv() {
            pending.push(block);
        }
        if !pending.is_empty() {
            pending.sort_by_key(|b| b.header.slot);
            info!(
                count = pending.len(),
                "catching up on pending blocks before leader slot"
            );
            for block in pending {
                self.replay_or_buffer_block(block)?;
            }
            self.process_orphan_queue()?;
        }

        // 2. Wait for the previous slot's block if it's missing.
        let prev_slot = self.current_slot.saturating_sub(1);
        if prev_slot > 0
            && !self.replay_stage.fork_tree().contains(prev_slot)
            && !self.am_i_leader(prev_slot)
        {
            let wait_ms = DEFAULT_SLOT_DURATION_MS / 2;
            tracing::debug!(
                slot = self.current_slot,
                prev_slot,
                wait_ms,
                "waiting for previous slot's block before producing"
            );
            match tokio::time::timeout(Duration::from_millis(wait_ms), block_rx.recv()).await {
                Ok(Some(block)) => {
                    self.replay_or_buffer_block(block)?;
                    // Drain any additional blocks that arrived
                    while let Ok(extra) = block_rx.try_recv() {
                        self.replay_or_buffer_block(extra)?;
                    }
                    self.process_orphan_queue()?;
                }
                Ok(None) => return Err(ValidatorError::Shutdown),
                Err(_) => {
                    tracing::debug!(
                        slot = self.current_slot,
                        prev_slot,
                        "previous slot block didn't arrive, producing anyway"
                    );
                }
            }
        }

        // 3. Skip production if this slot was already processed
        if self.replay_stage.fork_tree().contains(self.current_slot) {
            info!(
                slot = self.current_slot,
                "slot already in fork tree, skipping production"
            );
            return Ok(());
        }

        // 3a. Set parent to the fork-choice best fork before producing.
        let best = self.replay_stage.fork_tree().best_slot();
        if let Some(node) = self.replay_stage.fork_tree().get_node(best) {
            let prev_parent = self.block_producer.parent_slot();
            if prev_parent != best {
                tracing::info!(
                    prev_parent,
                    best_fork = best,
                    "switching parent to fork-choice best fork"
                );
            }
            self.block_producer
                .set_parent(best, node.block_hash, node.bank_hash);
        }

        // 3c. Rebuild slot_hashes and rewind account index from fork tree ancestry,
        //     but ONLY when switching forks. On a linear chain the bank's slot_hashes
        //     already contain the correct history (record_slot_hash appends each slot).
        //     Replacing unconditionally would shrink slot_hashes to the fork tree's
        //     pruned ancestry (often just 1 entry on a single-node validator), causing
        //     all transactions with a recent blockhash to fail with BlockhashNotFound.
        let parent_slot = self.block_producer.parent_slot();
        let is_linear_extension = self.last_produced_slot == Some(parent_slot);
        let ancestry = self.replay_stage.fork_tree().get_ancestry(parent_slot);
        if !is_linear_extension {
            let fork_slot_hashes: Vec<(u64, Hash)> = ancestry
                .iter()
                .filter_map(|&s| {
                    self.replay_stage
                        .fork_tree()
                        .get_node(s)
                        .map(|n| (s, n.block_hash))
                })
                .collect();
            let merged = helpers::build_merged_slot_hashes(&fork_slot_hashes, &self.storage, 512);
            self.bank.set_slot_hashes(SlotHashes(merged));

            let ancestor_set: HashSet<u64> = ancestry.iter().copied().collect();
            let rewound = self
                .storage
                .rewind_account_index_for_ancestry(&ancestor_set)?;
            if rewound > 0 {
                tracing::info!(
                    parent_slot,
                    rewound,
                    "rewound account index (fork-aware) before production"
                );
            }
        }

        // 3b. Drain pending transactions from the priority mempool
        let transactions = self.mempool.drain_by_priority(self.max_txs_per_slot);

        // 4. Produce block
        let (block, exec_result, pending_storage) = self
            .block_producer
            .produce_block(self.current_slot, transactions)?;

        // Wrap in Arc to avoid expensive deep clones of the full block
        let block = Arc::new(block);

        // Mark our own block as stored
        self.shred_collector.mark_slot_stored(self.current_slot);

        // 5. Atomic block storage — put_block + put_slot_meta + bank/slot hashes
        //    all combined into a single StorageWriteBatch, offloaded to a
        //    blocking thread so RocksDB I/O doesn't stall the async slot loop.
        //    The result is awaited so write failures are detected and broadcast
        //    is skipped on error (preventing silent data loss).
        //    pending_storage is moved (not cloned) into the closure to avoid
        //    copying the FrozenBankState and SlotMeta allocations.
        let storage_write_ok = {
            let storage = self.storage.clone();
            let block_for_storage = Arc::clone(&block);
            let slot = self.current_slot;
            // Move pending_storage into the closure; all fields are accessed there.
            let PendingBlockStorage { frozen, slot_meta: sm } = pending_storage;
            tokio::task::spawn_blocking(move || -> Result<(), nusantara_storage::StorageError> {
                let mut batch = StorageWriteBatch::new();

                // put_block: header in CF_BLOCKS + full block in CF_DEFAULT
                let header_value = borsh::to_vec(&block_for_storage.header)
                    .map_err(|e| nusantara_storage::StorageError::Serialization(e.to_string()))?;
                let block_key = [b"block_".as_slice(), &slot_key(slot)].concat();
                let block_value = borsh::to_vec(&*block_for_storage)
                    .map_err(|e| nusantara_storage::StorageError::Serialization(e.to_string()))?;
                batch.put(CF_BLOCKS, slot_key(slot).to_vec(), header_value);
                batch.put(CF_DEFAULT, block_key, block_value);

                // put_slot_meta
                let sm_value = borsh::to_vec(&sm)
                    .map_err(|e| nusantara_storage::StorageError::Serialization(e.to_string()))?;
                batch.put(CF_SLOT_META, slot_key(slot).to_vec(), sm_value);

                // flush_to_storage: bank_hash + slot_hash
                batch.put(
                    CF_BANK_HASHES,
                    slot_key(frozen.slot).to_vec(),
                    frozen.bank_hash.as_bytes().to_vec(),
                );
                batch.put(
                    CF_SLOT_HASHES,
                    slot_key(frozen.slot).to_vec(),
                    frozen.block_hash.as_bytes().to_vec(),
                );

                storage.write(&batch)
            })
            .await
        };

        match storage_write_ok {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(error = %e, slot = self.current_slot, "atomic block storage write failed — skipping broadcast");
                // Storage write failed: the block is NOT persisted.
                // Skip broadcast to avoid propagating a block we can't serve.
                metrics::counter!("nusantara_block_storage_errors").increment(1);
                return Ok(());
            }
            Err(e) => {
                tracing::error!(error = %e, slot = self.current_slot, "block storage task panicked — skipping broadcast");
                metrics::counter!("nusantara_block_storage_errors").increment(1);
                return Ok(());
            }
        }

        // 6. Feed into ReplayStage for fork tree tracking.
        //    poh_entries is empty for leader-produced blocks (PoH is trusted locally),
        //    so parent_poh is not read by replay_block — Hash::zero() is safe here
        //    and avoids a storage read on the hot leader path.
        let result = self.replay_stage.replay_block(&block, &[], &Hash::zero())?;
        self.replay_tip_shared
            .store(self.current_slot, Ordering::Relaxed);

        // Defer root advancement (leader is never catching up for its own blocks)
        if let Some(root) = result.new_root {
            self.try_advance_root(root, false)?;
        }

        // 7. Publish pubsub events immediately after replay (before broadcast)
        //    so that send-and-confirm / airdrop-and-confirm endpoints get notified
        //    without waiting for Turbine shredding and network I/O.
        let root = self.storage.get_latest_root().ok().flatten().unwrap_or(0);
        if let Err(e) = self.pubsub_tx.send(PubsubEvent::SlotUpdate {
            slot: self.current_slot,
            parent: block.header.parent_slot,
            root,
        }) {
            tracing::debug!(error = %e, "pubsub SlotUpdate send failed (no subscribers)");
        }
        if let Err(e) = self.pubsub_tx.send(PubsubEvent::BlockNotification {
            slot: self.current_slot,
            block_hash: block.header.block_hash.to_base58(),
            tx_count: block.header.transaction_count,
        }) {
            tracing::debug!(error = %e, "pubsub BlockNotification send failed (no subscribers)");
        }

        // Publish SignatureNotification using inline tx_statuses (no RocksDB reads)
        for (tx_hash, status_str) in &exec_result.tx_statuses {
            let sig_b58 = tx_hash.to_base58();
            let _ = self.pubsub_tx.send(PubsubEvent::SignatureNotification {
                signature: sig_b58,
                slot: self.current_slot,
                status: status_str.clone(),
            });
        }

        // 8. Build TurbineTree and broadcast in background (after notifications)
        {
            let block_for_broadcast = Arc::clone(&block);
            let identity = self.identity;
            let ci = Arc::clone(&self.cluster_info);
            let bank = Arc::clone(&self.bank);
            let current_slot = self.current_slot;
            let broadcast = broadcast.clone();
            tokio::spawn(async move {
                let mut peers: Vec<Hash> = ci.all_peers().iter().map(|ci| ci.identity()).collect();
                if !peers.contains(&identity) {
                    peers.push(identity);
                }
                let stakes_vec = bank.get_stake_distribution();
                let stakes: std::collections::HashMap<Hash, u64> = stakes_vec.into_iter().collect();
                let tree = TurbineTree::new(
                    identity,
                    &peers,
                    &stakes,
                    current_slot,
                    TURBINE_FANOUT as usize,
                );
                let ci2 = ci.clone();
                if let Err(e) = broadcast
                    .broadcast_block(&block_for_broadcast, &tree, |id| {
                        ci2.get_contact_info(id).map(|c| c.turbine_addr.0)
                    })
                    .await
                {
                    tracing::warn!(error = %e, slot = current_slot, "background broadcast failed");
                }
            });
        }

        // Track parent for linear-extension detection (skip rewind next slot)
        self.last_produced_slot = Some(self.current_slot);

        metrics::counter!("nusantara_leader_slots").increment(1);
        info!(
            slot = self.current_slot,
            fork_tree_nodes = self.replay_stage.fork_tree().node_count(),
            fork_tree_root = self.replay_stage.fork_tree().root_slot(),
            "leader slot completed"
        );
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(slot = self.current_slot))]
    async fn non_leader_slot(
        &mut self,
        block_rx: &mut mpsc::Receiver<Block>,
        leader_timeout_ms: u64,
        catching_up: bool,
    ) -> Result<(), ValidatorError> {
        let mut blocks = Vec::new();

        if leader_timeout_ms == 0 {
            // Catch-up mode: non-blocking drain only, no waiting.
            while let Ok(block) = block_rx.try_recv() {
                blocks.push(block);
            }
        } else {
            // Steady state: wait for at least one block with timeout.
            let timeout = Duration::from_millis(leader_timeout_ms);
            match tokio::time::timeout(timeout, block_rx.recv()).await {
                Ok(Some(block)) => blocks.push(block),
                Ok(None) => return Err(ValidatorError::Shutdown),
                Err(_) => {} // timeout — no block arrived
            }
            // Drain additional available blocks (non-blocking)
            while let Ok(block) = block_rx.try_recv() {
                blocks.push(block);
            }
        }

        if blocks.is_empty() {
            if !catching_up {
                let skips = self.consecutive_skips.fetch_add(1, Ordering::Relaxed) + 1;
                self.total_skips += 1;
                warn!(
                    slot = self.current_slot,
                    consecutive_skips = skips,
                    "no block received (leader skip)"
                );
                if skips > 10 {
                    warn!(
                        consecutive_skips = skips,
                        "possible network partition — many consecutive leader skips"
                    );
                }
                metrics::counter!("nusantara_leader_skips").increment(1);
                metrics::gauge!("nusantara_consecutive_skips").set(skips as f64);
            }
            metrics::counter!("nusantara_non_leader_slots").increment(1);
            return Ok(());
        }

        // Sort by slot for correct replay order
        blocks.sort_by_key(|b| b.header.slot);
        metrics::gauge!("nusantara_blocks_drained_per_slot").set(blocks.len() as f64);

        for block in blocks {
            self.replay_or_buffer_block(block)?;
        }

        self.process_orphan_queue()?;

        // During catch-up, do a second drain pass — replaying blocks may
        // have unblocked orphans, and new blocks may have arrived from
        // repair/retransmit while we were replaying.
        if catching_up {
            let mut extra = Vec::new();
            while let Ok(block) = block_rx.try_recv() {
                extra.push(block);
            }
            if !extra.is_empty() {
                extra.sort_by_key(|b| b.header.slot);
                metrics::counter!("nusantara_catchup_blocks_replayed")
                    .increment(extra.len() as u64);
                for block in extra {
                    self.replay_or_buffer_block(block)?;
                }
                self.process_orphan_queue()?;
            }
            // Yield to let background services (repair, retransmit) run
            if self.current_slot.is_multiple_of(64) {
                tokio::task::yield_now().await;
            }
        }

        metrics::counter!("nusantara_non_leader_slots").increment(1);
        Ok(())
    }
}
