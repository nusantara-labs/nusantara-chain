use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{Hash, Keypair};

use crate::contact_info::ContactInfo;
use crate::crds::CrdsTable;
use crate::crds_value::{CrdsData, CrdsValue};
use crate::protocol::{PullRequest, PullResponse};

pub const MAX_PULL_RESPONSE_VALUES: u64 =
    const_parse_u64(env!("NUSA_GOSSIP_MAX_PULL_RESPONSE_VALUES"));

/// Maximum pull response size in bytes (UDP-safe, below 65507 MTU).
const MAX_PULL_RESPONSE_SIZE: usize = 65000;

pub struct CrdsGossipPull {
    my_identity: Hash,
}

impl CrdsGossipPull {
    pub fn new(my_identity: Hash) -> Self {
        Self { my_identity }
    }

    /// Build a pull request: bloom filter of our CRDS labels + our self-value.
    pub fn build_pull_request(
        &self,
        crds: &CrdsTable,
        keypair: &Keypair,
        self_contact_info: &ContactInfo,
    ) -> PullRequest {
        let bloom = crds.cached_bloom(0.1);

        let self_value =
            CrdsValue::new_signed(CrdsData::ContactInfo(self_contact_info.clone()), keypair);

        PullRequest {
            filter: bloom,
            self_value,
        }
    }

    /// Process a pull request: return values not in the requester's bloom filter.
    /// Uses size-aware accumulation to prevent MTU overflow.
    pub fn process_pull_request(&self, crds: &CrdsTable, request: &PullRequest) -> PullResponse {
        if !request.filter.is_valid() {
            metrics::counter!("nusantara_gossip_pull_invalid_bloom_total").increment(1);
            return PullResponse {
                from: self.my_identity,
                values: vec![],
            };
        }

        let all_values = crds.values_since(0);

        let mut response_values = Vec::new();
        let mut total_size: usize = 0;

        for v in all_values {
            let label_bytes = borsh::to_vec(&v.label()).unwrap_or_default();
            if request.filter.contains(&label_bytes) {
                continue;
            }

            let value_size = crds_value_serialized_size(&v);
            if total_size + value_size > MAX_PULL_RESPONSE_SIZE {
                metrics::counter!("nusantara_gossip_pull_response_truncated_total").increment(1);
                break;
            }
            if response_values.len() >= MAX_PULL_RESPONSE_VALUES as usize {
                break;
            }

            total_size += value_size;
            response_values.push(v);
        }

        PullResponse {
            from: self.my_identity,
            values: response_values,
        }
    }

    /// Process a pull response: verify and insert values into our CRDS table.
    /// Short-circuits stale values before signature verification (L19).
    pub fn process_pull_response(&self, crds: &CrdsTable, response: &PullResponse) -> usize {
        let mut inserted = 0;
        for value in &response.values {
            // Skip sig verify for values we already have at equal or newer wallclock (L19).
            if crds
                .get(&value.label())
                .map(|existing| existing.wallclock() >= value.wallclock())
                .unwrap_or(false)
            {
                continue;
            }

            let pubkey = match &value.data {
                CrdsData::ContactInfo(ci) => Some(ci.pubkey.clone()),
                _ => crds
                    .get_contact_info(&value.origin())
                    .map(|ci| ci.pubkey.clone()),
            };

            match &pubkey {
                Some(pk) => {
                    if !value.verify(pk) {
                        metrics::counter!("nusantara_gossip_pull_invalid_signature_total")
                            .increment(1);
                        continue;
                    }
                }
                None => {
                    metrics::counter!("nusantara_gossip_unverifiable_value_dropped_total")
                        .increment(1);
                    continue;
                }
            }

            if crds.insert(value.clone()).is_ok() {
                inserted += 1;
            }
        }
        if inserted > 0 {
            metrics::counter!("nusantara_gossip_pull_values_received_total")
                .increment(inserted as u64);
        }
        inserted
    }
}

/// Compute the exact Borsh-serialized size of a CRDS value (M6).
/// Uses borsh::object_length for accuracy; adds 8 bytes overhead margin.
fn crds_value_serialized_size(value: &CrdsValue) -> usize {
    borsh::object_length(value).unwrap_or(0) + 8
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::Keypair;

    fn make_contact_info(kp: &Keypair) -> ContactInfo {
        ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            1000,
        )
    }

    #[test]
    fn config_values() {
        assert_eq!(MAX_PULL_RESPONSE_VALUES, 20);
    }

    #[test]
    fn build_pull_request() {
        let kp = Keypair::generate();
        let pull = CrdsGossipPull::new(kp.address());
        let crds = CrdsTable::new();
        let ci = make_contact_info(&kp);

        let req = pull.build_pull_request(&crds, &kp, &ci);
        assert!(req.self_value.verify(kp.public_key()));
    }

    #[test]
    fn pull_returns_missing_values() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();

        let crds1 = CrdsTable::new();
        let pull1 = CrdsGossipPull::new(kp1.address());

        let crds2 = CrdsTable::new();
        let pull2 = CrdsGossipPull::new(kp2.address());
        let ci2 = make_contact_info(&kp2);
        crds2
            .insert(CrdsValue::new_signed(
                CrdsData::ContactInfo(ci2.clone()),
                &kp2,
            ))
            .unwrap();

        let ci1 = make_contact_info(&kp1);
        let req = pull1.build_pull_request(&crds1, &kp1, &ci1);

        let resp = pull2.process_pull_request(&crds2, &req);
        assert!(!resp.values.is_empty());

        let inserted = pull1.process_pull_response(&crds1, &resp);
        assert!(inserted > 0);
        assert!(!crds1.is_empty());
    }

    #[test]
    fn pull_filters_existing_values() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();

        let crds1 = CrdsTable::new();
        let crds2 = CrdsTable::new();

        let ci = make_contact_info(&kp2);
        let value = CrdsValue::new_signed(CrdsData::ContactInfo(ci.clone()), &kp2);
        crds1.insert(value.clone()).unwrap();
        crds2.insert(value).unwrap();

        let pull1 = CrdsGossipPull::new(kp1.address());
        let pull2 = CrdsGossipPull::new(kp2.address());

        let ci1 = make_contact_info(&kp1);
        let req = pull1.build_pull_request(&crds1, &kp1, &ci1);
        let resp = pull2.process_pull_request(&crds2, &req);

        assert!(resp.values.is_empty());
    }

    #[test]
    fn pull_response_rejects_unverifiable_non_contact_info() {
        let kp1 = Keypair::generate();
        let unknown_kp = Keypair::generate();
        let crds = CrdsTable::new();
        let pull = CrdsGossipPull::new(kp1.address());

        use crate::crds_value::CrdsVote;
        let vote = CrdsVote {
            from: unknown_kp.address(),
            slot: 1,
            hash: Hash::zero(),
            wallclock: 1000,
        };
        let value = CrdsValue::new_signed(CrdsData::Vote(vote), &unknown_kp);

        let resp = PullResponse {
            from: unknown_kp.address(),
            values: vec![value],
        };

        let inserted = pull.process_pull_response(&crds, &resp);
        assert_eq!(inserted, 0);
    }

    #[test]
    fn stale_value_skips_sig_verify() {
        let kp = Keypair::generate();
        let crds = CrdsTable::new();
        let pull = CrdsGossipPull::new(kp.address());

        // Insert value at wallclock 2000.
        let ci = make_contact_info(&kp);
        let mut ci_new = ci.clone();
        ci_new.wallclock = 2000;
        let v_new = CrdsValue::new_signed(CrdsData::ContactInfo(ci_new), &kp);
        crds.insert(v_new).unwrap();

        // Attempt to pull a stale value (wallclock 1000 < 2000).
        let v_old = CrdsValue::new_signed(CrdsData::ContactInfo(ci), &kp);
        let resp = PullResponse {
            from: kp.address(),
            values: vec![v_old],
        };

        // Must be skipped (stale), not inserted.
        let inserted = pull.process_pull_response(&crds, &resp);
        assert_eq!(inserted, 0);
    }

    #[test]
    fn crds_value_size_is_exact() {
        let kp = Keypair::generate();
        let ci = make_contact_info(&kp);
        let value = CrdsValue::new_signed(CrdsData::ContactInfo(ci), &kp);
        let actual = borsh::to_vec(&value).unwrap().len();
        let estimated = crds_value_serialized_size(&value);
        // Estimated must be >= actual (never under-counts).
        assert!(
            estimated >= actual,
            "estimate {estimated} < actual {actual}"
        );
    }
}
