use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;

use borsh::{BorshDeserialize, BorshSerialize};

use crate::error::CryptoError;
use crate::hash::Hash;

pub type Address = Hash;

const SUFFIX: &str = ".nusantara";
const MAX_ACCOUNT_LEN: usize = 128;
const MIN_SEGMENT_LEN: usize = 2;
const MAX_SEGMENT_LEN: usize = 63;

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum AccountId {
    Named(String),
    Implicit(Address),
}

impl BorshSerialize for AccountId {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        match self {
            AccountId::Named(name) => {
                0u8.serialize(writer)?;
                name.serialize(writer)
            }
            AccountId::Implicit(addr) => {
                1u8.serialize(writer)?;
                addr.serialize(writer)
            }
        }
    }
}

impl BorshDeserialize for AccountId {
    fn deserialize_reader<R: io::Read>(reader: &mut R) -> io::Result<Self> {
        let tag: u8 = BorshDeserialize::deserialize_reader(reader)?;
        match tag {
            0 => {
                let name: String = BorshDeserialize::deserialize_reader(reader)?;
                Ok(AccountId::Named(name))
            }
            1 => {
                let addr: Hash = BorshDeserialize::deserialize_reader(reader)?;
                Ok(AccountId::Implicit(addr))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid AccountId tag: {tag}"),
            )),
        }
    }
}

impl AccountId {
    pub fn named(name: &str) -> Result<Self, CryptoError> {
        validate_named(name)?;
        Ok(Self::Named(name.to_string()))
    }

    pub fn implicit(address: Address) -> Self {
        Self::Implicit(address)
    }

    pub fn is_named(&self) -> bool {
        matches!(self, Self::Named(_))
    }

    pub fn is_implicit(&self) -> bool {
        matches!(self, Self::Implicit(_))
    }

    pub fn is_top_level(&self) -> bool {
        match self {
            Self::Named(name) => {
                let prefix = name.strip_suffix(SUFFIX).unwrap_or(name);
                !prefix.contains('.')
            }
            Self::Implicit(_) => false,
        }
    }

    pub fn is_sub_account_of(&self, parent: &str) -> bool {
        match self {
            Self::Named(name) => {
                name.len() > parent.len()
                    && name.ends_with(parent)
                    && name.as_bytes()[name.len() - parent.len() - 1] == b'.'
            }
            Self::Implicit(_) => false,
        }
    }

    pub fn parent(&self) -> Option<AccountId> {
        match self {
            Self::Named(name) => {
                let prefix = name.strip_suffix(SUFFIX)?;
                let dot_pos = prefix.find('.')?;
                let parent_name = &name[dot_pos + 1..];
                Some(Self::Named(parent_name.to_string()))
            }
            Self::Implicit(_) => None,
        }
    }
}

fn validate_named(name: &str) -> Result<(), CryptoError> {
    let err = |msg: &str| CryptoError::InvalidAccountId(msg.to_string());

    if name.len() > MAX_ACCOUNT_LEN {
        return Err(err(&format!(
            "account id too long: {} > {MAX_ACCOUNT_LEN}",
            name.len()
        )));
    }

    let prefix = name
        .strip_suffix(SUFFIX)
        .ok_or_else(|| err(&format!("must end with '{SUFFIX}'")))?;

    if prefix.is_empty() {
        return Err(err("empty account name before suffix"));
    }

    for segment in prefix.split('.') {
        validate_segment(segment)?;
    }

    Ok(())
}

fn validate_segment(segment: &str) -> Result<(), CryptoError> {
    let err = |msg: &str| CryptoError::InvalidAccountId(msg.to_string());

    if segment.len() < MIN_SEGMENT_LEN || segment.len() > MAX_SEGMENT_LEN {
        return Err(err(&format!(
            "segment '{segment}' length {} not in [{MIN_SEGMENT_LEN}, {MAX_SEGMENT_LEN}]",
            segment.len()
        )));
    }

    if segment.starts_with('-')
        || segment.ends_with('-')
        || segment.starts_with('_')
        || segment.ends_with('_')
    {
        return Err(err(&format!(
            "segment '{segment}' cannot start or end with '-' or '_'"
        )));
    }

    for ch in segment.chars() {
        if !matches!(ch, 'a'..='z' | '0'..='9' | '-' | '_') {
            return Err(err(&format!(
                "invalid character '{ch}' in segment '{segment}'"
            )));
        }
    }

    Ok(())
}

impl fmt::Debug for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Named(name) => write!(f, "AccountId({name})"),
            Self::Implicit(addr) => write!(f, "AccountId({addr})"),
        }
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Named(name) => f.write_str(name),
            Self::Implicit(addr) => write!(f, "{addr}"),
        }
    }
}

impl FromStr for AccountId {
    type Err = CryptoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.ends_with(SUFFIX) {
            Self::named(s)
        } else {
            let hash = Hash::from_base64(s)?;
            Ok(Self::Implicit(hash))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_named_accounts() {
        assert!(AccountId::named("alice.nusantara").is_ok());
        assert!(AccountId::named("bob99.nusantara").is_ok());
        assert!(AccountId::named("my-account.nusantara").is_ok());
        assert!(AccountId::named("dex.alice.nusantara").is_ok());
    }

    #[test]
    fn invalid_named_accounts() {
        assert!(AccountId::named("alice").is_err());
        assert!(AccountId::named(".nusantara").is_err());
        assert!(AccountId::named("a.nusantara").is_err()); // too short
        assert!(AccountId::named("-bad.nusantara").is_err());
        assert!(AccountId::named("bad-.nusantara").is_err());
        assert!(AccountId::named("BAD.nusantara").is_err());
    }

    #[test]
    fn top_level_detection() {
        let alice = AccountId::named("alice.nusantara").unwrap();
        assert!(alice.is_top_level());

        let dex = AccountId::named("dex.alice.nusantara").unwrap();
        assert!(!dex.is_top_level());
    }

    #[test]
    fn sub_account_of() {
        let dex = AccountId::named("dex.alice.nusantara").unwrap();
        assert!(dex.is_sub_account_of("alice.nusantara"));
        assert!(!dex.is_sub_account_of("bob.nusantara"));
    }

    #[test]
    fn parent() {
        let dex = AccountId::named("dex.alice.nusantara").unwrap();
        let parent = dex.parent().unwrap();
        assert_eq!(parent.to_string(), "alice.nusantara");

        let alice = AccountId::named("alice.nusantara").unwrap();
        assert!(alice.parent().is_none());
    }

    #[test]
    fn display_fromstr_roundtrip_named() {
        let acc = AccountId::named("alice.nusantara").unwrap();
        let s = acc.to_string();
        let parsed: AccountId = s.parse().unwrap();
        assert_eq!(acc, parsed);
    }

    #[test]
    fn display_fromstr_roundtrip_implicit() {
        let hash = crate::hash::hash(b"some pubkey bytes");
        let acc = AccountId::Implicit(hash);
        let s = acc.to_string();
        let parsed: AccountId = s.parse().unwrap();
        assert_eq!(acc, parsed);
    }

    #[test]
    fn borsh_roundtrip() {
        let named = AccountId::named("alice.nusantara").unwrap();
        let encoded = borsh::to_vec(&named).unwrap();
        let decoded: AccountId = borsh::from_slice(&encoded).unwrap();
        assert_eq!(named, decoded);

        let implicit = AccountId::Implicit(crate::hash::hash(b"test"));
        let encoded = borsh::to_vec(&implicit).unwrap();
        let decoded: AccountId = borsh::from_slice(&encoded).unwrap();
        assert_eq!(implicit, decoded);
    }
}
