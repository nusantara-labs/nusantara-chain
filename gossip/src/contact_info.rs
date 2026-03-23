use std::net::SocketAddr;

use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::{Hash, PublicKey};

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ContactInfo {
    pub identity: Hash,
    pub pubkey: PublicKey,
    pub gossip_addr: SocketAddrBorsh,
    pub tpu_addr: SocketAddrBorsh,
    pub tpu_forward_addr: SocketAddrBorsh,
    pub turbine_addr: SocketAddrBorsh,
    pub repair_addr: SocketAddrBorsh,
    pub shred_version: u16,
    pub wallclock: u64,
}

impl ContactInfo {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pubkey: PublicKey,
        gossip_addr: SocketAddr,
        tpu_addr: SocketAddr,
        tpu_forward_addr: SocketAddr,
        turbine_addr: SocketAddr,
        repair_addr: SocketAddr,
        shred_version: u16,
        wallclock: u64,
    ) -> Self {
        let identity = nusantara_crypto::hash(pubkey.as_bytes());
        Self {
            identity,
            pubkey,
            gossip_addr: SocketAddrBorsh(gossip_addr),
            tpu_addr: SocketAddrBorsh(tpu_addr),
            tpu_forward_addr: SocketAddrBorsh(tpu_forward_addr),
            turbine_addr: SocketAddrBorsh(turbine_addr),
            repair_addr: SocketAddrBorsh(repair_addr),
            shred_version,
            wallclock,
        }
    }
}

/// Borsh-serializable wrapper for SocketAddr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketAddrBorsh(pub SocketAddr);

impl BorshSerialize for SocketAddrBorsh {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        match self.0 {
            SocketAddr::V4(v4) => {
                0u8.serialize(writer)?;
                v4.ip().octets().serialize(writer)?;
                v4.port().serialize(writer)?;
            }
            SocketAddr::V6(v6) => {
                1u8.serialize(writer)?;
                v6.ip().octets().serialize(writer)?;
                v6.port().serialize(writer)?;
            }
        }
        Ok(())
    }
}

impl BorshDeserialize for SocketAddrBorsh {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let tag = u8::deserialize_reader(reader)?;
        match tag {
            0 => {
                let octets = <[u8; 4]>::deserialize_reader(reader)?;
                let port = u16::deserialize_reader(reader)?;
                let addr = SocketAddr::from((octets, port));
                Ok(Self(addr))
            }
            1 => {
                let octets = <[u8; 16]>::deserialize_reader(reader)?;
                let port = u16::deserialize_reader(reader)?;
                let addr = SocketAddr::from((octets, port));
                Ok(Self(addr))
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid SocketAddr tag: {tag}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::Keypair;

    fn test_contact_info() -> ContactInfo {
        let kp = Keypair::generate();
        let addr: SocketAddr = "127.0.0.1:8000".parse().unwrap();
        ContactInfo::new(
            kp.public_key().clone(),
            addr,
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        )
    }

    #[test]
    fn identity_is_hash_of_pubkey() {
        let ci = test_contact_info();
        assert_eq!(ci.identity, nusantara_crypto::hash(ci.pubkey.as_bytes()));
    }

    #[test]
    fn borsh_roundtrip() {
        let ci = test_contact_info();
        let bytes = borsh::to_vec(&ci).unwrap();
        let decoded: ContactInfo = borsh::from_slice(&bytes).unwrap();
        assert_eq!(ci, decoded);
    }

    #[test]
    fn socket_addr_v6_roundtrip() {
        let addr: SocketAddr = "[::1]:8000".parse().unwrap();
        let wrapped = SocketAddrBorsh(addr);
        let bytes = borsh::to_vec(&wrapped).unwrap();
        let decoded: SocketAddrBorsh = borsh::from_slice(&bytes).unwrap();
        assert_eq!(wrapped, decoded);
    }
}
