use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use nusantara_core::Transaction;
use nusantara_crypto::Hash;
use nusantara_gossip::GossipService;
use nusantara_rpc::{RpcServer, RpcState, RpcTlsConfig};
use nusantara_consensus::leader_schedule::LeaderScheduleGenerator;
use nusantara_tpu_forward::TpuService;
use nusantara_turbine::protocol::{RepairRequest, MAX_UDP_PACKET};
use nusantara_turbine::repair_service::MAX_REPAIR_BATCH_REQUEST;
use nusantara_turbine::turbine_tree::TURBINE_FANOUT;
use nusantara_turbine::{
    BatchRepairResponse, BroadcastStage, MerkleShred, RepairService, RetransmitStage,
    ShredReceiver, Shredder, TurbineMessage, TurbineTree,
};
use std::sync::atomic::Ordering;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tracing::info;

use nusantara_core::block::Block;

use crate::cli::Cli;
use crate::error::ValidatorError;
use crate::node::ValidatorNode;

/// Outputs from spawning all background services.
pub(crate) struct SpawnedServices {
    pub block_rx: mpsc::Receiver<Block>,
    pub broadcast_stage: BroadcastStage,
    pub current_slot_shared: Arc<AtomicU64>,
    pub shutdown_tx: watch::Sender<bool>,
    /// Background service tasks; monitored for unexpected exits.
    pub service_tasks: JoinSet<&'static str>,
}

impl ValidatorNode {
    pub(crate) async fn spawn_services(
        &self,
        cli: &Cli,
    ) -> Result<SpawnedServices, ValidatorError> {
        // 1. Shutdown channel
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut service_tasks: JoinSet<&'static str> = JoinSet::new();

        // 2. Spawn GossipService
        let gossip_service =
            GossipService::new(Arc::clone(&self.cluster_info), self.gossip_addr).await?;
        let gossip_shutdown = shutdown_rx.clone();
        service_tasks.spawn(async move {
            gossip_service.run(gossip_shutdown).await;
            "gossip"
        });
        info!(addr = %self.gossip_addr, "gossip service started");

        // 3. Spawn Turbine pipeline
        let turbine_socket = Arc::new(UdpSocket::bind(self.turbine_addr).await?);
        let repair_socket = Arc::new(UdpSocket::bind(self.repair_addr).await?);
        info!(
            turbine = %turbine_socket.local_addr()?,
            repair = %repair_socket.local_addr()?,
            "turbine sockets bound"
        );

        let shred_collector = Arc::clone(&self.shred_collector);

        // Shared current-slot counter
        let current_slot_shared = Arc::new(AtomicU64::new(self.current_slot));

        // Channels
        let (shred_tx, shred_rx) = mpsc::channel(10_000);
        let repair_shred_tx = shred_tx.clone();
        let (repair_msg_tx, _repair_msg_rx) = mpsc::channel(1_000);
        let (block_tx, block_rx) = mpsc::channel(100);

        // 3a. ShredReceiver
        let shred_receiver = ShredReceiver::new(Arc::clone(&turbine_socket));
        let shred_shutdown = shutdown_rx.clone();
        service_tasks.spawn(async move {
            shred_receiver
                .run(shred_tx, repair_msg_tx, shred_shutdown)
                .await;
            "shred_receiver"
        });

        // 3b. RetransmitStage
        let retransmit = RetransmitStage::new(
            self.identity,
            Arc::clone(&turbine_socket),
            Arc::clone(&shred_collector),
            Arc::clone(&current_slot_shared),
        );
        let retransmit_shutdown = shutdown_rx.clone();

        // tree_provider closure
        let tree_leader_cache = Arc::clone(&self.leader_cache);
        let tree_cluster_info = Arc::clone(&self.cluster_info);
        let tree_bank = Arc::clone(&self.bank);
        let tree_epoch_schedule = self.epoch_schedule.clone();
        let tree_identity = self.identity;
        let tree_provider = move |slot: u64| -> Option<TurbineTree> {
            let epoch = tree_epoch_schedule.get_epoch(slot);
            let cache = tree_leader_cache.read();
            let leader = *cache.get(&epoch)?.get_leader(slot, &tree_epoch_schedule)?;
            let mut peers: Vec<Hash> = tree_cluster_info
                .all_peers()
                .iter()
                .map(|ci| ci.identity)
                .collect();
            if !peers.contains(&tree_identity) {
                peers.push(tree_identity);
            }
            let stakes_vec = tree_bank.get_stake_distribution();
            let stakes: std::collections::HashMap<Hash, u64> = stakes_vec.into_iter().collect();
            Some(TurbineTree::new(
                leader,
                &peers,
                &stakes,
                slot,
                TURBINE_FANOUT as usize,
            ))
        };

        // addr_lookup closure
        let retransmit_ci = Arc::clone(&self.cluster_info);
        let addr_lookup = move |id: &Hash| -> Option<SocketAddr> {
            retransmit_ci
                .get_contact_info(id)
                .map(|ci| ci.turbine_addr.0)
        };

        // pubkey_lookup closure
        let pubkey_ci = Arc::clone(&self.cluster_info);
        let pubkey_lookup =
            move |id: &Hash| -> Option<nusantara_crypto::PublicKey> { pubkey_ci.get_pubkey(id) };

        service_tasks.spawn(async move {
            retransmit
                .run(
                    shred_rx,
                    block_tx,
                    tree_provider,
                    addr_lookup,
                    pubkey_lookup,
                    retransmit_shutdown,
                )
                .await;
            "retransmit"
        });

        // 3c. RepairService
        let repair_service = RepairService::new(
            Arc::clone(&repair_socket),
            Arc::clone(&shred_collector),
            Arc::clone(&current_slot_shared),
        );
        let repair_shutdown = shutdown_rx.clone();
        let repair_ci = Arc::clone(&self.cluster_info);
        let my_identity = self.identity;
        let repair_peers_fn = move || -> Vec<SocketAddr> {
            repair_ci
                .all_peers()
                .iter()
                .filter(|ci| ci.identity != my_identity)
                .map(|ci| ci.repair_addr.0)
                .collect()
        };
        service_tasks.spawn(async move {
            repair_service.run(repair_peers_fn, repair_shutdown).await;
            "repair"
        });

        // 3d. Repair responder
        spawn_repair_responder(
            Arc::clone(&repair_socket),
            Arc::clone(&self.storage),
            Arc::clone(&self.keypair),
            repair_shred_tx,
            shutdown_rx.clone(),
        );
        info!("turbine pipeline started");

        // 4. Spawn TpuService
        let server_config = TpuService::create_server_config()?;
        let client_config = TpuService::create_client_config()?;

        let server_endpoint = quinn::Endpoint::server(server_config, self.tpu_addr)?;
        let mut client_endpoint =
            quinn::Endpoint::client("0.0.0.0:0".parse::<SocketAddr>().unwrap())?;
        client_endpoint.set_default_client_config(client_config);

        let tpu_identity = self.identity;
        let tpu_shutdown = shutdown_rx.clone();

        // Bridge channel: TPU writes to mpsc, background task drains into mempool.
        let (tpu_tx_sender, mut tpu_tx_receiver) = mpsc::channel::<Transaction>(10_000);
        let rpc_tx_forward_sender = tpu_tx_sender.clone();
        let tpu_mempool = Arc::clone(&self.mempool);
        let mut tpu_bridge_shutdown = shutdown_rx.clone();
        service_tasks.spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    result = tpu_bridge_shutdown.changed() => {
                        if result.is_ok() {
                            while let Ok(tx) = tpu_tx_receiver.try_recv() {
                                let _ = tpu_mempool.insert(tx);
                            }
                        }
                        break;
                    }
                    Some(tx) = tpu_tx_receiver.recv() => {
                        if let Err(e) = tpu_mempool.insert(tx) {
                            tracing::debug!(error = %e, "TPU bridge: mempool rejected transaction");
                        }
                    }
                }
            }
            info!("TPU-mempool bridge stopped");
            "tpu_bridge"
        });

        // leader_lookup closure for TPU
        let tpu_leader_cache = Arc::clone(&self.leader_cache);
        let tpu_cluster_info = Arc::clone(&self.cluster_info);
        let tpu_epoch_schedule = self.epoch_schedule.clone();
        let tpu_current_slot = Arc::clone(&current_slot_shared);

        let leader_lookup = move || -> Option<(Hash, SocketAddr)> {
            let slot = tpu_current_slot.load(Ordering::Relaxed);
            let epoch = tpu_epoch_schedule.get_epoch(slot);
            let cache = tpu_leader_cache.read();
            let leader = cache.get(&epoch)?.get_leader(slot, &tpu_epoch_schedule)?;
            let addr = tpu_cluster_info
                .get_contact_info(leader)?
                .tpu_forward_addr
                .0;
            Some((*leader, addr))
        };

        service_tasks.spawn(async move {
            TpuService::run(
                server_endpoint,
                client_endpoint,
                tpu_identity,
                tpu_tx_sender,
                leader_lookup,
                tpu_shutdown,
            )
            .await;
            "tpu"
        });
        info!(addr = %self.tpu_addr, "TPU service started");

        // 5. Spawn RPC server
        let rpc_addr: SocketAddr = cli
            .rpc_addr
            .parse()
            .map_err(|e| ValidatorError::NetworkInit(format!("invalid rpc addr: {e}")))?;

        let faucet_keypair = if cli.enable_faucet {
            nusantara_genesis::load_faucet_keypair(&self.storage)
                .map(Arc::new)
                .or_else(|| {
                    tracing::warn!("no faucet keypair in genesis, falling back to validator identity");
                    Some(Arc::clone(&self.keypair))
                })
        } else {
            None
        };

        let rpc_state = Arc::new(RpcState {
            storage: Arc::clone(&self.storage),
            bank: Arc::clone(&self.bank),
            mempool: Arc::clone(&self.mempool),
            leader_cache: Arc::clone(&self.leader_cache),
            leader_schedule_generator: LeaderScheduleGenerator::new(self.epoch_schedule.clone()),
            epoch_schedule: self.epoch_schedule.clone(),
            genesis_hash: self.genesis_hash,
            faucet_keypair,
            identity: self.identity,
            cluster_info: Arc::clone(&self.cluster_info),
            consecutive_skips: Arc::clone(&self.consecutive_skips),
            tx_forward_sender: Some(rpc_tx_forward_sender),
            pubsub_tx: self.pubsub_tx.clone(),
            snapshot_dir: Path::new(&cli.ledger_path).join("snapshots"),
            ws_semaphore: RpcState::new_ws_semaphore(),
            faucet_address_cooldowns: Default::default(),
            faucet_ip_cooldowns: Default::default(),
        });

        // Build optional TLS config from CLI flags
        let rpc_tls = match (&cli.rpc_tls_cert, &cli.rpc_tls_key) {
            (Some(cert_path), Some(key_path)) => {
                let tls = RpcTlsConfig::from_pem_files(Path::new(cert_path), Path::new(key_path))
                    .map_err(|e| ValidatorError::NetworkInit(format!("RPC TLS init: {e}")))?;
                info!(cert = cert_path, key = key_path, "RPC TLS enabled");
                Some(tls)
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(ValidatorError::NetworkInit(
                    "both --rpc-tls-cert and --rpc-tls-key must be provided".to_string(),
                ));
            }
            _ => None,
        };

        let rpc_shutdown = shutdown_rx.clone();
        service_tasks.spawn(async move {
            RpcServer::serve(rpc_addr, rpc_state, rpc_tls, rpc_shutdown).await;
            "rpc"
        });
        info!(addr = %rpc_addr, "RPC server started");

        // 6. Create BroadcastStage (called on-demand by leader path)
        let broadcast_stage =
            BroadcastStage::new(Arc::clone(&self.keypair), Arc::clone(&turbine_socket));

        Ok(SpawnedServices {
            block_rx,
            broadcast_stage,
            current_slot_shared,
            shutdown_tx,
            service_tasks,
        })
    }
}

/// Standalone repair responder task.
fn spawn_repair_responder(
    socket: Arc<UdpSocket>,
    storage: Arc<nusantara_storage::Storage>,
    keypair: Arc<nusantara_crypto::Keypair>,
    shred_tx: mpsc::Sender<(TurbineMessage, SocketAddr)>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_UDP_PACKET];
        let shred_cache: Arc<parking_lot::Mutex<lru::LruCache<u64, Arc<nusantara_turbine::ShredBatch>>>> =
            Arc::new(parking_lot::Mutex::new(lru::LruCache::new(
                std::num::NonZero::new(64).unwrap(),
            )));
        loop {
            tokio::select! {
                biased;
                result = socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, src)) => {
                            let data = &buf[..len];
                            match TurbineMessage::deserialize_from_bytes(data) {
                                Ok(TurbineMessage::RepairRequest(request)) => {
                                    metrics::counter!("nusantara_turbine_repair_requests_received")
                                        .increment(1);
                                    let slot = match &request {
                                        RepairRequest::Shred { slot, .. }
                                        | RepairRequest::ShredBatch { slot, .. }
                                        | RepairRequest::HighestShred { slot }
                                        | RepairRequest::Orphan { slot }
                                        | RepairRequest::BatchHeader { slot } => *slot,
                                    };
                                    tracing::debug!(slot, ?request, %src, "received repair request");

                                    // Check LRU cache first, then fetch + re-shred on miss.
                                    // Lock scope is kept tight to avoid holding across .await.
                                    let cached = shred_cache.lock().get(&slot).cloned();
                                    let shred_batch = if let Some(batch) = cached {
                                        metrics::counter!("nusantara_turbine_repair_cache_hits").increment(1);
                                        batch
                                    } else {
                                        // Offload blocking RocksDB read to spawn_blocking
                                        let storage_clone = storage.clone();
                                        let keypair_clone = Arc::clone(&keypair);
                                        let result = tokio::task::spawn_blocking(move || {
                                            let block = storage_clone.get_block(slot).ok()??;
                                            let batch = Shredder::shred_block(
                                                &block,
                                                block.header.parent_slot,
                                                &keypair_clone,
                                            ).ok()?;
                                            Some(Arc::new(batch))
                                        }).await;
                                        let batch = match result {
                                            Ok(Some(b)) => b,
                                            _ => continue,
                                        };
                                        shred_cache.lock().put(slot, Arc::clone(&batch));
                                        metrics::counter!("nusantara_turbine_repair_cache_misses").increment(1);
                                        batch
                                    };
                                    match request {
                                        RepairRequest::Shred { index, .. } => {
                                            if let Some(shred) =
                                                shred_batch.data_shreds.get(index as usize)
                                            {
                                                let msg = TurbineMessage::RepairResponse(
                                                    MerkleShred::Data(shred.clone()),
                                                );
                                                if let Ok(bytes) = msg.serialize_to_bytes() {
                                                    let _ = socket
                                                        .send_to(&bytes, src)
                                                        .await;
                                                }
                                            }
                                        }
                                        RepairRequest::ShredBatch { ref indices, .. } => {
                                            if indices.len() > MAX_REPAIR_BATCH_REQUEST as usize {
                                                tracing::warn!(
                                                    %slot,
                                                    count = indices.len(),
                                                    "repair request too large, dropping"
                                                );
                                                continue;
                                            }
                                            let shreds: Vec<MerkleShred> = indices
                                                .iter()
                                                .filter_map(|&i| {
                                                    shred_batch
                                                        .data_shreds
                                                        .get(i as usize)
                                                        .map(|s| MerkleShred::Data(s.clone()))
                                                })
                                                .collect();
                                            let batches = BatchRepairResponse::pack(
                                                slot,
                                                shreds,
                                                MAX_UDP_PACKET,
                                            );
                                            for batch in batches {
                                                let msg = TurbineMessage::BatchRepairResponse(
                                                    batch,
                                                );
                                                if let Ok(bytes) = msg.serialize_to_bytes() {
                                                    let _ = socket
                                                        .send_to(&bytes, src)
                                                        .await;
                                                }
                                            }
                                        }
                                        RepairRequest::BatchHeader { .. } => {
                                            let msg = TurbineMessage::ShredBatchHeader(
                                                shred_batch.header.clone(),
                                            );
                                            if let Ok(bytes) = msg.serialize_to_bytes() {
                                                let _ = socket
                                                    .send_to(&bytes, src)
                                                    .await;
                                            }
                                        }
                                        RepairRequest::HighestShred { .. }
                                        | RepairRequest::Orphan { .. } => {
                                            if let Some(last_shred) =
                                                shred_batch.data_shreds.last()
                                            {
                                                let msg = TurbineMessage::RepairResponse(
                                                    MerkleShred::Data(last_shred.clone()),
                                                );
                                                if let Ok(bytes) = msg.serialize_to_bytes() {
                                                    let _ = socket
                                                        .send_to(&bytes, src)
                                                        .await;
                                                }
                                            }
                                            let shreds: Vec<MerkleShred> = shred_batch
                                                .data_shreds
                                                .iter()
                                                .map(|s| MerkleShred::Data(s.clone()))
                                                .collect();
                                            let batches = BatchRepairResponse::pack(
                                                slot,
                                                shreds,
                                                MAX_UDP_PACKET,
                                            );
                                            for batch in batches {
                                                let msg = TurbineMessage::BatchRepairResponse(
                                                    batch,
                                                );
                                                if let Ok(bytes) = msg.serialize_to_bytes() {
                                                    let _ = socket
                                                        .send_to(&bytes, src)
                                                        .await;
                                                }
                                            }
                                        }
                                    }
                                    metrics::counter!("nusantara_turbine_repair_responses_sent")
                                        .increment(1);
                                }
                                Ok(TurbineMessage::RepairResponse(shred)) => {
                                    tracing::debug!(
                                        slot = shred.slot(),
                                        index = shred.index(),
                                        %src,
                                        "received repair response shred"
                                    );
                                    metrics::counter!("nusantara_turbine_repair_shreds_received")
                                        .increment(1);
                                    let msg = TurbineMessage::RepairResponse(shred);
                                    let _ = shred_tx.send((msg, src)).await;
                                }
                                Ok(TurbineMessage::BatchRepairResponse(batch)) => {
                                    tracing::debug!(
                                        slot = batch.slot,
                                        shred_count = batch.shreds.len(),
                                        %src,
                                        "received batch repair response"
                                    );
                                    let count = batch.shreds.len() as u64;
                                    metrics::counter!("nusantara_turbine_repair_shreds_received")
                                        .increment(count);
                                    for shred in batch.shreds {
                                        let msg = TurbineMessage::Shred(shred);
                                        let _ = shred_tx.send((msg, src)).await;
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "repair socket recv error");
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    break;
                }
            }
        }
        info!("repair responder stopped");
    });
}
