use std::fmt;

use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, Keypair, PublicKey, Signature};

use crate::contact_info::ContactInfo;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CrdsValue {
    pub data: CrdsData,
    pub signature: Signature,
}

impl CrdsValue {
    pub fn new_signed(data: CrdsData, keypair: &Keypair) -> Self {
        let serialized = borsh::to_vec(&data).expect("CrdsData serialization cannot fail");
        let signature = keypair.sign(&serialized);
        Self { data, signature }
    }

    pub fn verify(&self, pubkey: &PublicKey) -> bool {
        let serialized = match borsh::to_vec(&self.data) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        self.signature.verify(pubkey, &serialized).is_ok()
    }

    pub fn label(&self) -> CrdsValueLabel {
        match &self.data {
            CrdsData::ContactInfo(ci) => CrdsValueLabel::ContactInfo(ci.identity),
            CrdsData::Vote(v) => CrdsValueLabel::Vote(v.from, v.slot),
            CrdsData::EpochSlots(es) => CrdsValueLabel::EpochSlots(es.from),
            CrdsData::LowestSlot(ls) => CrdsValueLabel::LowestSlot(ls.from),
            CrdsData::SlashProof(sp) => CrdsValueLabel::SlashProof(sp.from, sp.validator, sp.slot),
        }
    }

    pub fn wallclock(&self) -> u64 {
        match &self.data {
            CrdsData::ContactInfo(ci) => ci.wallclock,
            CrdsData::Vote(v) => v.wallclock,
            CrdsData::EpochSlots(es) => es.wallclock,
            CrdsData::LowestSlot(ls) => ls.wallclock,
            CrdsData::SlashProof(sp) => sp.wallclock,
        }
    }

    pub fn origin(&self) -> Hash {
        match &self.data {
            CrdsData::ContactInfo(ci) => ci.identity,
            CrdsData::Vote(v) => v.from,
            CrdsData::EpochSlots(es) => es.from,
            CrdsData::LowestSlot(ls) => ls.from,
            CrdsData::SlashProof(sp) => sp.from,
        }
    }

    pub fn pubkey(&self) -> Option<&PublicKey> {
        match &self.data {
            CrdsData::ContactInfo(ci) => Some(&ci.pubkey),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum CrdsData {
    ContactInfo(ContactInfo),
    Vote(CrdsVote),
    EpochSlots(CrdsEpochSlots),
    LowestSlot(CrdsLowestSlot),
    SlashProof(CrdsSlashProof),
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CrdsVote {
    pub from: Hash,
    pub slot: u64,
    pub hash: Hash,
    pub wallclock: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CrdsEpochSlots {
    pub from: Hash,
    pub slots: Vec<u64>,
    pub wallclock: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CrdsLowestSlot {
    pub from: Hash,
    pub lowest_slot: u64,
    pub wallclock: u64,
}

/// Gossip-propagated proof of equivocation (double-voting) by a validator.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CrdsSlashProof {
    /// Identity of the reporter (node that detected the equivocation).
    pub from: Hash,
    /// Identity of the validator that double-voted.
    pub validator: Hash,
    /// Slot in which the equivocation occurred.
    pub slot: u64,
    /// Block hash from the first vote observed.
    pub vote1_hash: Hash,
    /// Block hash from the conflicting second vote.
    pub vote2_hash: Hash,
    /// Wallclock timestamp (millis since epoch) for CRDS freshness.
    pub wallclock: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, BorshSerialize, BorshDeserialize)]
pub enum CrdsValueLabel {
    ContactInfo(Hash),
    Vote(Hash, u64),
    EpochSlots(Hash),
    LowestSlot(Hash),
    /// (reporter, validator, slot) — uniquely identifies a slash proof.
    SlashProof(Hash, Hash, u64),
}

impl fmt::Display for CrdsValueLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContactInfo(id) => write!(f, "ContactInfo({id:?})"),
            Self::Vote(id, slot) => write!(f, "Vote({id:?}, {slot})"),
            Self::EpochSlots(id) => write!(f, "EpochSlots({id:?})"),
            Self::LowestSlot(id) => write!(f, "LowestSlot({id:?})"),
            Self::SlashProof(from, validator, slot) => {
                write!(f, "SlashProof({from:?}, {validator:?}, {slot})")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let kp = Keypair::generate();
        let ci = ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(CrdsData::ContactInfo(ci), &kp);
        assert!(value.verify(kp.public_key()));
    }

    #[test]
    fn wrong_key_fails_verify() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let ci = ContactInfo::new(
            kp1.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(CrdsData::ContactInfo(ci), &kp1);
        assert!(!value.verify(kp2.public_key()));
    }

    #[test]
    fn label_extraction() {
        let kp = Keypair::generate();
        let identity = kp.address();
        let ci = ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(CrdsData::ContactInfo(ci), &kp);
        assert_eq!(value.label(), CrdsValueLabel::ContactInfo(identity));
    }

    #[test]
    fn borsh_roundtrip() {
        let kp = Keypair::generate();
        let ci = ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        );
        let value = CrdsValue::new_signed(CrdsData::ContactInfo(ci), &kp);
        let bytes = borsh::to_vec(&value).unwrap();
        let decoded: CrdsValue = borsh::from_slice(&bytes).unwrap();
        assert_eq!(value, decoded);
    }
}
