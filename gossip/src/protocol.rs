use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, Signature};

use crate::bloom::BloomFilter;
use crate::crds_value::CrdsValue;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum GossipMessage {
    PullRequest(PullRequest),
    PullResponse(PullResponse),
    PushMessage(PushMessage),
    PruneMessage(PruneMessage),
    Ping(PingMessage),
    Pong(PongMessage),
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PullRequest {
    pub filter: BloomFilter,
    pub self_value: CrdsValue,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PullResponse {
    pub from: Hash,
    pub values: Vec<CrdsValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PushMessage {
    pub from: Hash,
    pub values: Vec<CrdsValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PruneMessage {
    pub from: Hash,
    pub prunes: Vec<Hash>,
    pub destination: Hash,
    pub wallclock: u64,
    pub signature: Signature,
}

/// PingMessage includes target and wallclock to prevent cross-peer replay (M10).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PingMessage {
    pub from: Hash,
    pub token: Hash,
    /// Identity of the intended recipient — bound into the signature to prevent replay.
    pub target: Hash,
    /// Sender wallclock (ms since epoch) — further binds the ping to a time window.
    pub wallclock: u64,
    pub signature: Signature,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PongMessage {
    pub from: Hash,
    pub token_hash: Hash,
    pub signature: Signature,
}

/// Maximum size of a serialized gossip message.
/// 65507 = maximum UDP payload over IPv4 (65535 - 20 IP header - 8 UDP header).
pub const MAX_GOSSIP_MESSAGE_SIZE: usize = 65507;

/// Maximum number of CRDS values allowed in a single PullResponse or PushMessage.
/// Lowered to 32 to limit Dilithium3 verify CPU cost per message (C4).
pub const MAX_GOSSIP_VALUES: usize = 32;

impl GossipMessage {
    pub fn serialize_to_bytes(&self) -> Result<Vec<u8>, String> {
        borsh::to_vec(self).map_err(|e| e.to_string())
    }

    pub fn deserialize_from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_GOSSIP_MESSAGE_SIZE {
            return Err(format!(
                "gossip message too large: {} bytes (max {})",
                bytes.len(),
                MAX_GOSSIP_MESSAGE_SIZE
            ));
        }

        let msg: Self = borsh::from_slice(bytes).map_err(|e| e.to_string())?;

        match &msg {
            GossipMessage::PullResponse(resp) if resp.values.len() > MAX_GOSSIP_VALUES => {
                return Err(format!(
                    "PullResponse contains too many values: {} (max {})",
                    resp.values.len(),
                    MAX_GOSSIP_VALUES
                ));
            }
            GossipMessage::PushMessage(push) if push.values.len() > MAX_GOSSIP_VALUES => {
                return Err(format!(
                    "PushMessage contains too many values: {} (max {})",
                    push.values.len(),
                    MAX_GOSSIP_VALUES
                ));
            }
            _ => {}
        }

        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::{Keypair, hash, hashv};

    #[test]
    fn ping_pong_roundtrip() {
        let kp = Keypair::generate();
        let token = hash(b"ping_token");
        let target = hash(b"target_peer");
        let wallclock = 12345u64;
        let sign_payload = hashv(&[
            b"ping",
            token.as_bytes(),
            target.as_bytes(),
            &wallclock.to_le_bytes(),
        ]);
        let sig = kp.sign(sign_payload.as_bytes());

        let ping = GossipMessage::Ping(PingMessage {
            from: kp.address(),
            token,
            target,
            wallclock,
            signature: sig,
        });

        let bytes = ping.serialize_to_bytes().unwrap();
        let decoded = GossipMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(ping, decoded);
    }

    #[test]
    fn push_message_roundtrip() {
        let kp = Keypair::generate();
        let ci = crate::contact_info::ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(crate::crds_value::CrdsData::ContactInfo(ci), &kp);
        let msg = GossipMessage::PushMessage(PushMessage {
            from: kp.address(),
            values: vec![value],
        });

        let bytes = msg.serialize_to_bytes().unwrap();
        let decoded = GossipMessage::deserialize_from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn reject_oversized_message() {
        let oversized = vec![0u8; MAX_GOSSIP_MESSAGE_SIZE + 1];
        let result = GossipMessage::deserialize_from_bytes(&oversized);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("too large"),
            "error should mention size limit"
        );
    }

    #[test]
    fn reject_push_message_with_too_many_values() {
        let kp = Keypair::generate();
        let ci = crate::contact_info::ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(crate::crds_value::CrdsData::ContactInfo(ci), &kp);

        // Layer 2 test: craft raw Borsh bytes with values count > MAX_GOSSIP_VALUES
        // within size budget using a bogus length prefix.
        // Borsh Vec = 4-byte LE length + elements.
        // PushMessage variant tag = 2.
        let mut crafted = Vec::new();
        crafted.push(2u8); // PushMessage variant
        crafted.extend_from_slice(&[0u8; 64]); // `from` Hash (64 bytes)
        let bogus_count = (MAX_GOSSIP_VALUES + 1) as u32;
        crafted.extend_from_slice(&bogus_count.to_le_bytes());
        // Incomplete elements — Borsh will reject (IO error or values count check).
        let result = GossipMessage::deserialize_from_bytes(&crafted);
        assert!(
            result.is_err(),
            "crafted truncated message must be rejected"
        );

        // Layer 1 test: real message with MAX_GOSSIP_VALUES+1 entries exceeds size limit.
        let values: Vec<CrdsValue> = std::iter::repeat_n(value, MAX_GOSSIP_VALUES + 1).collect();
        let msg = GossipMessage::PushMessage(PushMessage {
            from: kp.address(),
            values,
        });
        let bytes = msg.serialize_to_bytes().unwrap();
        let result = GossipMessage::deserialize_from_bytes(&bytes);
        assert!(result.is_err(), "oversized push message must be rejected");
    }

    #[test]
    fn reject_pull_response_with_too_many_values() {
        let mut crafted = Vec::new();
        crafted.push(1u8); // PullResponse variant
        crafted.extend_from_slice(&[0u8; 64]); // `from` Hash
        let bogus_count = (MAX_GOSSIP_VALUES + 1) as u32;
        crafted.extend_from_slice(&bogus_count.to_le_bytes());
        let result = GossipMessage::deserialize_from_bytes(&crafted);
        assert!(
            result.is_err(),
            "crafted oversized PullResponse must be rejected"
        );
    }
}
