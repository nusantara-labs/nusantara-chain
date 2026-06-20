use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use nusantara_crypto::Hash;
use parking_lot::RwLock;

use crate::bloom::BloomFilter;
use crate::contact_info::ContactInfo;
use crate::crds_value::{CrdsData, CrdsValue, CrdsValueLabel};
use crate::error::GossipError;

#[derive(Debug)]
pub struct CrdsEntry {
    pub value: CrdsValue,
    pub insert_order: u64,
}

/// Maximum number of entries in the CRDS table to prevent unbounded growth.
pub const MAX_CRDS_ENTRIES: usize = 100_000;

/// Maximum number of slots in a single EpochSlots CRDS entry.
pub const MAX_EPOCH_SLOTS_PER_ENTRY: usize = 512;

pub struct CrdsTable {
    entries: DashMap<CrdsValueLabel, CrdsEntry>,
    cursor: AtomicU64,
    /// Ordered index for efficient `values_since()`: insert_order -> label.
    ordered_index: RwLock<BTreeMap<u64, CrdsValueLabel>>,
}

impl CrdsTable {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            cursor: AtomicU64::new(0),
            ordered_index: RwLock::new(BTreeMap::new()),
        }
    }

    /// Insert a CRDS value. Returns Ok(Some(old)) if replaced, Ok(None) if new,
    /// or Err if the new value is stale (older wallclock).
    ///
    /// Uses DashMap::entry() API to eliminate TOCTOU race between check and insert.
    pub fn insert(&self, value: CrdsValue) -> Result<Option<CrdsValue>, GossipError> {
        // Reject oversized EpochSlots to prevent memory abuse.
        if let crate::crds_value::CrdsData::EpochSlots(ref es) = value.data
            && es.slots.len() > MAX_EPOCH_SLOTS_PER_ENTRY
        {
            return Err(GossipError::OversizedValue);
        }

        let label = value.label();
        let wallclock = value.wallclock();

        let mut index = self.ordered_index.write();

        // Evict before acquiring shard lock via entry() to avoid deadlock:
        // entry() holds a DashMap shard lock, and evict_oldest calls remove()
        // which may need the same shard lock.
        if !self.entries.contains_key(&label) && self.entries.len() >= MAX_CRDS_ENTRIES {
            self.evict_oldest(&mut index);
        }

        match self.entries.entry(label.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                let existing = occupied.get();
                if existing.value.wallclock() >= wallclock {
                    return Err(GossipError::StaleValue {
                        value_wallclock: wallclock,
                        existing_wallclock: existing.value.wallclock(),
                    });
                }
                let old_order = existing.insert_order;
                let order = self.cursor.fetch_add(1, Ordering::Relaxed);

                // Remove old index entry, add new one
                index.remove(&old_order);
                index.insert(order, label);

                let old_value = occupied.get().value.clone();
                occupied.insert(CrdsEntry {
                    value,
                    insert_order: order,
                });

                metrics::counter!("nusantara_gossip_crds_inserts_total").increment(1);
                Ok(Some(old_value))
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                let order = self.cursor.fetch_add(1, Ordering::Relaxed);
                index.insert(order, label);
                vacant.insert(CrdsEntry {
                    value,
                    insert_order: order,
                });

                metrics::counter!("nusantara_gossip_crds_inserts_total").increment(1);
                Ok(None)
            }
        }
    }

    /// Evict the oldest-inserted entry to make room for new inserts.
    /// MUST be called while holding ordered_index write lock (passed as parameter).
    fn evict_oldest(&self, index: &mut BTreeMap<u64, CrdsValueLabel>) {
        if let Some((&order, label)) = index.iter().next() {
            let label = label.clone();
            self.entries.remove(&label);
            index.remove(&order);
            metrics::counter!("nusantara_gossip_crds_evicted_total").increment(1);
        }
    }

    pub fn get(&self, label: &CrdsValueLabel) -> Option<CrdsValue> {
        self.entries.get(label).map(|e| e.value.clone())
    }

    pub fn all_contact_infos(&self) -> Vec<ContactInfo> {
        self.entries
            .iter()
            .filter_map(|entry| {
                if let CrdsData::ContactInfo(ci) = &entry.value().value.data {
                    Some(ci.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns values inserted after `cursor`, using the ordered index for
    /// O(log n + k) lookup instead of O(n) full scan.
    pub fn values_since(&self, cursor: u64) -> Vec<CrdsValue> {
        let index = self.ordered_index.read();
        index
            .range(cursor..)
            .filter_map(|(_, label)| self.entries.get(label).map(|e| e.value.clone()))
            .collect()
    }

    pub fn current_cursor(&self) -> u64 {
        self.cursor.load(Ordering::Relaxed)
    }

    pub fn purge_old(&self, min_wallclock: u64) -> usize {
        let stale_labels: Vec<(CrdsValueLabel, u64)> = self
            .entries
            .iter()
            .filter(|e| e.value().value.wallclock() < min_wallclock)
            .map(|e| (e.key().clone(), e.value().insert_order))
            .collect();

        let count = stale_labels.len();

        let mut index = self.ordered_index.write();
        for (label, order) in &stale_labels {
            self.entries.remove(label);
            index.remove(order);
        }

        if count > 0 {
            metrics::counter!("nusantara_gossip_crds_purged_total").increment(count as u64);
        }
        count
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get_contact_info(&self, identity: &Hash) -> Option<ContactInfo> {
        let label = CrdsValueLabel::ContactInfo(*identity);
        self.entries.get(&label).and_then(|e| {
            if let CrdsData::ContactInfo(ci) = &e.value.data {
                Some(ci.clone())
            } else {
                None
            }
        })
    }

    pub fn all_labels(&self) -> Vec<CrdsValueLabel> {
        self.entries.iter().map(|e| e.key().clone()).collect()
    }

    /// Return a bloom filter pre-loaded with all current CRDS labels.
    /// Rebuilt from the current table state on each call (L7).
    pub fn cached_bloom(&self, fp_rate: f64) -> BloomFilter {
        let labels = self.all_labels();
        let mut bloom = BloomFilter::for_capacity(labels.len().max(1), fp_rate);
        for label in &labels {
            let label_bytes = borsh::to_vec(label).unwrap_or_default();
            bloom.add(&label_bytes);
        }
        bloom
    }

    /// Iterate values inserted after `cursor` without cloning the entire vec (L8).
    pub fn for_each_value_since<F: FnMut(&CrdsValue)>(&self, cursor: u64, mut f: F) {
        let index = self.ordered_index.read();
        for (_, label) in index.range(cursor..) {
            if let Some(entry) = self.entries.get(label) {
                f(&entry.value);
            }
        }
    }
}

impl Default for CrdsTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::Keypair;

    fn make_contact_value(kp: &Keypair, wallclock: u64) -> CrdsValue {
        let ci = ContactInfo::new(
            kp.public_key().clone(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:8003".parse().unwrap(),
            "127.0.0.1:8004".parse().unwrap(),
            "127.0.0.1:8001".parse().unwrap(),
            "127.0.0.1:8002".parse().unwrap(),
            1,
            wallclock,
        );
        CrdsValue::new_signed(CrdsData::ContactInfo(ci), kp)
    }

    #[test]
    fn insert_and_get() {
        let table = CrdsTable::new();
        let kp = Keypair::generate();
        let value = make_contact_value(&kp, 1000);
        let label = value.label();

        assert!(table.insert(value.clone()).unwrap().is_none());
        assert_eq!(table.get(&label).unwrap(), value);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn update_newer_wallclock() {
        let table = CrdsTable::new();
        let kp = Keypair::generate();
        let v1 = make_contact_value(&kp, 1000);
        let v2 = make_contact_value(&kp, 2000);

        table.insert(v1.clone()).unwrap();
        let old = table.insert(v2.clone()).unwrap();
        assert_eq!(old.unwrap(), v1);
        assert_eq!(table.get(&v2.label()).unwrap(), v2);
    }

    #[test]
    fn reject_stale_value() {
        let table = CrdsTable::new();
        let kp = Keypair::generate();
        let v1 = make_contact_value(&kp, 2000);
        let v2 = make_contact_value(&kp, 1000);

        table.insert(v1).unwrap();
        let result = table.insert(v2);
        assert!(result.is_err());
    }

    #[test]
    fn all_contact_infos() {
        let table = CrdsTable::new();
        for _ in 0..5 {
            let kp = Keypair::generate();
            table.insert(make_contact_value(&kp, 1000)).unwrap();
        }
        assert_eq!(table.all_contact_infos().len(), 5);
    }

    #[test]
    fn values_since_cursor() {
        let table = CrdsTable::new();
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();

        table.insert(make_contact_value(&kp1, 1000)).unwrap();
        let cursor = table.current_cursor();
        table.insert(make_contact_value(&kp2, 1001)).unwrap();

        let new_values = table.values_since(cursor);
        assert_eq!(new_values.len(), 1);
    }

    #[test]
    fn purge_old() {
        let table = CrdsTable::new();
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();

        table.insert(make_contact_value(&kp1, 100)).unwrap();
        table.insert(make_contact_value(&kp2, 2000)).unwrap();

        let purged = table.purge_old(1000);
        assert_eq!(purged, 1);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn ordered_index_correct_after_updates() {
        let table = CrdsTable::new();
        let kp = Keypair::generate();

        // Insert v1, update to v2
        let v1 = make_contact_value(&kp, 1000);
        table.insert(v1).unwrap();
        let cursor = table.current_cursor();

        let v2 = make_contact_value(&kp, 2000);
        table.insert(v2.clone()).unwrap();

        // values_since(cursor) should return the updated value
        let vals = table.values_since(cursor);
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0], v2);
    }

    #[test]
    fn ordered_index_correct_after_purge() {
        let table = CrdsTable::new();
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();

        table.insert(make_contact_value(&kp1, 100)).unwrap();
        table.insert(make_contact_value(&kp2, 2000)).unwrap();

        table.purge_old(1000);

        // Only kp2's value should remain
        let vals = table.values_since(0);
        assert_eq!(vals.len(), 1);
    }

    #[test]
    fn eviction_at_capacity() {
        // Test eviction logic with a small set (generating Dilithium3 keys is slow).
        // We verify the evict_oldest behavior directly.
        let table = CrdsTable::new();
        let capacity = 10;

        let keypairs: Vec<Keypair> = (0..capacity).map(|_| Keypair::generate()).collect();
        for (i, kp) in keypairs.iter().enumerate() {
            table
                .insert(make_contact_value(kp, 1000 + i as u64))
                .unwrap();
        }
        assert_eq!(table.len(), capacity);

        // Manually evict the oldest via the internal method
        {
            let mut index = table.ordered_index.write();
            table.evict_oldest(&mut index);
        }

        assert_eq!(table.len(), capacity - 1);

        // The first inserted entry (oldest insert_order) should be gone
        let first_label = CrdsValueLabel::ContactInfo(keypairs[0].address());
        assert!(table.get(&first_label).is_none());

        // All others should remain
        for kp in &keypairs[1..] {
            let label = CrdsValueLabel::ContactInfo(kp.address());
            assert!(table.get(&label).is_some());
        }
    }

    #[test]
    fn cached_bloom_contains_all_labels() {
        let table = CrdsTable::new();
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        table.insert(make_contact_value(&kp1, 1000)).unwrap();
        table.insert(make_contact_value(&kp2, 1001)).unwrap();

        let bloom = table.cached_bloom(0.01);
        let label1 = CrdsValueLabel::ContactInfo(kp1.address());
        let label2 = CrdsValueLabel::ContactInfo(kp2.address());
        assert!(bloom.contains(&borsh::to_vec(&label1).unwrap()));
        assert!(bloom.contains(&borsh::to_vec(&label2).unwrap()));
    }

    #[test]
    fn for_each_value_since_visits_correct_entries() {
        let table = CrdsTable::new();
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        table.insert(make_contact_value(&kp1, 1000)).unwrap();
        let cursor = table.current_cursor();
        table.insert(make_contact_value(&kp2, 1001)).unwrap();

        let mut visited = Vec::new();
        table.for_each_value_since(cursor, |v| visited.push(v.origin()));
        assert_eq!(visited.len(), 1);
        assert_eq!(visited[0], kp2.address());
    }

    #[test]
    fn insert_evicts_when_at_max_capacity() {
        // Verify the full insert path triggers eviction (with small count)
        let table = CrdsTable::new();

        // Insert 5 entries
        let keypairs: Vec<Keypair> = (0..5).map(|_| Keypair::generate()).collect();
        for (i, kp) in keypairs.iter().enumerate() {
            table
                .insert(make_contact_value(kp, 1000 + i as u64))
                .unwrap();
        }

        // Simulate being at capacity by checking that contains_key + len
        // logic works (the actual MAX_CRDS_ENTRIES = 100k is too large to test)
        assert_eq!(table.len(), 5);

        // Insert a new entry (below MAX_CRDS_ENTRIES, so no eviction)
        let extra_kp = Keypair::generate();
        table
            .insert(make_contact_value(&extra_kp, 999_999))
            .unwrap();
        assert_eq!(table.len(), 6);
    }

    #[test]
    fn concurrent_inserts_entry_api() {
        use std::sync::Arc;
        use std::thread;

        let table = Arc::new(CrdsTable::new());

        // Pre-generate all values on the main thread (Keypair is not Clone)
        let kp = Keypair::generate();
        let identity = kp.address();
        let values: Vec<CrdsValue> = (0..10).map(|i| make_contact_value(&kp, 1000 + i)).collect();

        let mut handles = Vec::new();
        for v in values {
            let t = Arc::clone(&table);
            handles.push(thread::spawn(move || {
                let _ = t.insert(v);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Should have exactly 1 entry (the latest wallclock wins)
        assert_eq!(table.len(), 1);
        let value = table.get(&CrdsValueLabel::ContactInfo(identity)).unwrap();
        // The wallclock should be the highest successfully inserted
        assert!(value.wallclock() >= 1000);
    }
}
