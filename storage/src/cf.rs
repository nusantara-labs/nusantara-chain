use nusantara_core::native_token::const_parse_u64;
use rocksdb::{BlockBasedOptions, Cache, ColumnFamilyDescriptor, Options, SliceTransform};

const WRITE_BUFFER_SIZE_MB: u64 = const_parse_u64(env!("NUSA_ROCKSDB_WRITE_BUFFER_SIZE_MB"));
pub(crate) const MAX_BACKGROUND_JOBS: u64 =
    const_parse_u64(env!("NUSA_ROCKSDB_MAX_BACKGROUND_JOBS"));
const BLOOM_FILTER_BITS: u64 = const_parse_u64(env!("NUSA_ROCKSDB_BLOOM_FILTER_BITS"));
const BLOCK_CACHE_SIZE_MB: u64 = const_parse_u64(env!("NUSA_ROCKSDB_BLOCK_CACHE_SIZE_MB"));

pub const CF_DEFAULT: &str = "default";
pub const CF_ACCOUNTS: &str = "accounts";
pub const CF_ACCOUNT_INDEX: &str = "account_index";
pub const CF_BLOCKS: &str = "blocks";
pub const CF_TRANSACTIONS: &str = "transactions";
pub const CF_ADDRESS_SIGNATURES: &str = "address_signatures";
pub const CF_SLOT_META: &str = "slot_meta";
pub const CF_DATA_SHREDS: &str = "data_shreds";
pub const CF_CODE_SHREDS: &str = "code_shreds";
pub const CF_BANK_HASHES: &str = "bank_hashes";
pub const CF_ROOTS: &str = "roots";
pub const CF_SLOT_HASHES: &str = "slot_hashes";
pub const CF_SYSVARS: &str = "sysvars";
pub const CF_SNAPSHOTS: &str = "snapshots";
pub const CF_OWNER_INDEX: &str = "owner_index";
pub const CF_PROGRAM_INDEX: &str = "program_index";
pub const CF_SLASHES: &str = "slashes";

/// Hash size in bytes (SHA3-512 = 64 bytes).
const HASH_BYTES: usize = 64;

/// Slot size in bytes (u64 big-endian = 8 bytes).
const SLOT_BYTES: usize = 8;

pub const ALL_CF_NAMES: &[&str] = &[
    CF_DEFAULT,
    CF_ACCOUNTS,
    CF_ACCOUNT_INDEX,
    CF_BLOCKS,
    CF_TRANSACTIONS,
    CF_ADDRESS_SIGNATURES,
    CF_SLOT_META,
    CF_DATA_SHREDS,
    CF_CODE_SHREDS,
    CF_BANK_HASHES,
    CF_ROOTS,
    CF_SLOT_HASHES,
    CF_SYSVARS,
    CF_SNAPSHOTS,
    CF_OWNER_INDEX,
    CF_PROGRAM_INDEX,
    CF_SLASHES,
];

/// Column families that benefit from bloom filters (point-lookup heavy).
const BLOOM_FILTER_CFS: &[&str] = &[
    CF_ACCOUNTS,
    CF_ACCOUNT_INDEX,
    CF_TRANSACTIONS,
    CF_BANK_HASHES,
    CF_ROOTS,
    CF_SLOT_HASHES,
    CF_SYSVARS,
    CF_SNAPSHOTS,
    CF_OWNER_INDEX,
    CF_PROGRAM_INDEX,
    // CF_SLASHES uses prefix iteration by validator hash but also needs
    // point-lookups via get_slash_proof(validator, slot); a bloom filter
    // reduces unnecessary SST reads for validators with no recorded slashes.
    CF_SLASHES,
];

/// Create a shared block cache for all CFs.
pub fn shared_block_cache() -> Cache {
    Cache::new_lru_cache(BLOCK_CACHE_SIZE_MB as usize * 1024 * 1024)
}

pub fn cf_descriptors() -> Vec<ColumnFamilyDescriptor> {
    let cache = shared_block_cache();

    ALL_CF_NAMES
        .iter()
        .map(|name| {
            let mut opts = Options::default();
            opts.set_write_buffer_size((WRITE_BUFFER_SIZE_MB as usize) * 1024 * 1024);

            // Shared block cache
            let mut block_opts = BlockBasedOptions::default();
            block_opts.set_block_cache(&cache);

            // Bloom filters for point-lookup CFs
            if BLOOM_FILTER_CFS.contains(name) {
                block_opts.set_bloom_filter(BLOOM_FILTER_BITS as f64, false);
            }
            opts.set_block_based_table_factory(&block_opts);

            match *name {
                CF_ACCOUNTS => {
                    // Key: Hash(64) ++ slot(8) — prefix by address for iteration
                    opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(HASH_BYTES));
                }
                CF_ADDRESS_SIGNATURES => {
                    // Key: Hash(64) ++ slot(8) ++ tx_index(4) — prefix by address
                    opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(HASH_BYTES));
                }
                CF_DATA_SHREDS | CF_CODE_SHREDS => {
                    // Key: slot(8) ++ shred_index(4) — prefix by slot
                    opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(SLOT_BYTES));
                }
                CF_OWNER_INDEX | CF_PROGRAM_INDEX => {
                    // Key: owner/program_hash(64) ++ account_address(64) — prefix by owner/program
                    opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(HASH_BYTES));
                }
                CF_SLASHES => {
                    // Key: validator_hash(64) ++ slot(8 BE) — prefix by validator
                    opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(HASH_BYTES));
                }
                _ => {}
            }
            ColumnFamilyDescriptor::new(*name, opts)
        })
        .collect()
}
