use std::net::SocketAddr;
use std::sync::Arc;

use nusantara_core::block::Block;
use nusantara_crypto::{Hash, Keypair};
use tokio::net::UdpSocket;
use tracing::{debug, info, instrument};

use crate::error::TurbineError;
use crate::merkle_shred::MerkleShred;
use crate::shredder::Shredder;
use crate::turbine_tree::TurbineTree;

#[derive(Clone)]
pub struct BroadcastStage {
    keypair: Arc<Keypair>,
    socket: Arc<UdpSocket>,
}

impl BroadcastStage {
    pub fn new(keypair: Arc<Keypair>, socket: Arc<UdpSocket>) -> Self {
        Self { keypair, socket }
    }

    /// Shred a block and broadcast to layer-0 turbine peers.
    /// Sends the ShredBatchHeader first, then individual Merkle shreds.
    #[instrument(skip(self, block, tree, addr_lookup), fields(slot = block.header.slot))]
    pub async fn broadcast_block<F>(
        &self,
        block: &Block,
        tree: &TurbineTree,
        addr_lookup: F,
    ) -> Result<(), TurbineError>
    where
        F: Fn(&Hash) -> Option<SocketAddr>,
    {
        let slot = block.header.slot;
        let parent_slot = block.header.parent_slot;

        let batch = Shredder::shred_block(block, parent_slot, &self.keypair)?;
        let peer_ids = tree.retransmit_peers(&self.keypair.address());
        let peer_addrs: Vec<SocketAddr> = peer_ids
            .iter()
            .filter_map(&addr_lookup)
            .collect();

        info!(
            slot,
            data_shreds = batch.data_shreds.len(),
            code_shreds = batch.code_shreds.len(),
            layer_0_peers = peer_addrs.len(),
            "broadcasting block shreds"
        );

        // Send ShredBatchHeader FIRST
        let header_msg = crate::protocol::TurbineMessage::ShredBatchHeader(batch.header.clone());
        if let Ok(bytes) = header_msg.serialize_to_bytes() {
            for addr in &peer_addrs {
                if let Err(e) = self.socket.send_to(&bytes, addr).await {
                    debug!(%addr, error = %e, "failed to send batch header");
                }
            }
        }

        // Pre-serialize all shred messages
        let mut serialized_shreds =
            Vec::with_capacity(batch.data_shreds.len() + batch.code_shreds.len());

        for shred in &batch.data_shreds {
            let msg = crate::protocol::TurbineMessage::Shred(MerkleShred::Data(shred.clone()));
            match msg.serialize_to_bytes() {
                Ok(bytes) => serialized_shreds.push(bytes),
                Err(e) => {
                    debug!(error = %e, "failed to serialize data shred message");
                }
            }
        }

        for shred in &batch.code_shreds {
            let msg = crate::protocol::TurbineMessage::Shred(MerkleShred::Code(shred.clone()));
            match msg.serialize_to_bytes() {
                Ok(bytes) => serialized_shreds.push(bytes),
                Err(e) => {
                    debug!(error = %e, "failed to serialize code shred message");
                }
            }
        }

        // Send all pre-serialized shreds to all layer-0 peers
        for bytes in &serialized_shreds {
            for addr in &peer_addrs {
                if let Err(e) = self.socket.send_to(bytes, addr).await {
                    debug!(%addr, error = %e, "failed to send shred");
                }
            }
        }

        metrics::counter!("nusantara_turbine_broadcast_total").increment(1);
        metrics::histogram!("nusantara_turbine_shreds_per_broadcast")
            .record((batch.data_shreds.len() + batch.code_shreds.len()) as f64);

        Ok(())
    }
}
