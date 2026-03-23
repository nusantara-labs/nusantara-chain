//! LRU cache for compiled WASM modules.
//!
//! Compilation (parsing + validation) of a WASM module is expensive relative to
//! execution. The [`ProgramCache`] keeps the most-recently-used compiled
//! [`Module`]s in memory so that repeated invocations of the same program skip
//! the compilation step entirely.
//!
//! ## Engine sharing
//!
//! A wasmi [`Engine`] is expensive to create and modules compiled with one
//! engine cannot be used with another. The [`ProgramCache`] owns a single
//! [`Engine`] instance, created once at construction time, and exposes it to
//! all callers via [`ProgramCache::engine()`].
//!
//! ## Bytecode-hash keying
//!
//! The cache is keyed by the SHA3-512 hash of the program bytecode rather than
//! the program's on-chain address. This provides automatic cache invalidation
//! on program upgrades: new bytecode produces a new hash, so the old cached
//! module is never served for upgraded programs. It also enables deduplication
//! when multiple addresses deploy identical bytecode.
//!
//! ## Thread safety
//!
//! Thread safety is provided by a [`parking_lot::Mutex`] around the inner LRU
//! map. The lock is held only for the duration of a single get/put operation --
//! never across an `.await` point -- so there is no risk of async deadlocks.
//! The [`Engine`] itself is `Send + Sync` and safe to share without locking.

use std::num::NonZeroUsize;

use lru::LruCache;
use parking_lot::Mutex;
use wasmi::{Engine, Module};

use nusantara_crypto::Hash;

use crate::config::PROGRAM_CACHE_CAPACITY;

/// Build the shared wasmi engine configuration.
///
/// Fuel metering is enabled so the executor can enforce compute-unit limits,
/// and floating-point instructions are disabled (deterministic execution).
fn build_engine() -> Engine {
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    config.floats(false);
    Engine::new(&config)
}

/// An LRU cache mapping bytecode hashes to compiled wasmi [`Module`]s.
///
/// The cache owns a single [`Engine`] instance shared across all compiled
/// modules. Callers obtain a reference to this engine via [`Self::engine()`]
/// and must use it for all `Module::new`, `Store::new`, and `Linker::new`
/// calls so that modules remain compatible with the stores that execute them.
pub struct ProgramCache {
    /// The shared wasmi engine. Created once at construction time.
    engine: Engine,
    /// LRU map from bytecode hash to compiled module.
    cache: Mutex<LruCache<Hash, Module>>,
}

impl ProgramCache {
    /// Create a new cache with the given capacity.
    ///
    /// A single [`Engine`] is created and shared by all cached modules.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero (guarded by `NonZeroUsize`).
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity)
            .unwrap_or_else(|| NonZeroUsize::new(1).expect("1 is non-zero"));
        Self {
            engine: build_engine(),
            cache: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Create a new cache using the default capacity from `config.toml`.
    pub fn with_default_capacity() -> Self {
        Self::new(PROGRAM_CACHE_CAPACITY)
    }

    /// Return a reference to the shared wasmi [`Engine`].
    ///
    /// All callers must use this engine for `Module::new`, `Store::new`, and
    /// `Linker::new` to ensure module compatibility.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Retrieve a compiled module by its bytecode hash.
    ///
    /// Promotes the entry to the head of the LRU list if present.
    pub fn get(&self, bytecode_hash: &Hash) -> Option<Module> {
        self.cache.lock().get(bytecode_hash).cloned()
    }

    /// Insert a compiled module keyed by its bytecode hash.
    ///
    /// If the cache is at capacity the least-recently-used entry is evicted.
    pub fn insert(&self, bytecode_hash: Hash, module: Module) {
        self.cache.lock().put(bytecode_hash, module);
    }

    /// Remove a specific module from the cache by its bytecode hash.
    pub fn invalidate(&self, bytecode_hash: &Hash) {
        self.cache.lock().pop(bytecode_hash);
    }

    /// Remove all entries from the cache.
    pub fn clear(&self) {
        self.cache.lock().clear();
    }

    /// Return the number of modules currently cached.
    pub fn len(&self) -> usize {
        self.cache.lock().len()
    }

    /// Return `true` if the cache contains no entries.
    pub fn is_empty(&self) -> bool {
        self.cache.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn new_cache_is_empty() {
        let cache = ProgramCache::new(10);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn invalidate_missing_key_is_noop() {
        let cache = ProgramCache::new(10);
        let key = hash(b"nonexistent");
        cache.invalidate(&key); // should not panic
        assert!(cache.is_empty());
    }

    #[test]
    fn clear_on_empty_is_noop() {
        let cache = ProgramCache::new(10);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn default_capacity() {
        let cache = ProgramCache::with_default_capacity();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn get_missing_returns_none() {
        let cache = ProgramCache::new(10);
        let key = hash(b"missing");
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn engine_is_available() {
        let cache = ProgramCache::new(10);
        // The engine should be usable for module compilation.
        let engine = cache.engine();
        // Attempting to compile invalid WASM should produce an error (not a
        // panic), proving the engine is correctly configured.
        let result = Module::new(engine, [0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]);
        // A minimal WASM header is parseable but has no exports -- that's OK,
        // the point is the engine works.
        assert!(result.is_ok());
    }

    #[test]
    fn insert_and_retrieve_by_bytecode_hash() {
        let cache = ProgramCache::new(10);
        let engine = cache.engine();

        // Compile a minimal WASM module.
        let wasm = [0x00u8, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
        let module = Module::new(engine, wasm).expect("should compile");

        // Key by bytecode hash (not by some program address).
        let bytecode_hash = hash(&wasm);
        cache.insert(bytecode_hash, module);

        assert_eq!(cache.len(), 1);
        assert!(cache.get(&bytecode_hash).is_some());

        // A different key should not match.
        let other_key = hash(b"other");
        assert!(cache.get(&other_key).is_none());
    }

    #[test]
    fn lru_eviction_works() {
        // Cache with capacity 2.
        let cache = ProgramCache::new(2);
        let engine = cache.engine();

        let wasm = [0x00u8, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
        let module = Module::new(engine, wasm).expect("should compile");

        let key1 = hash(b"key1");
        let key2 = hash(b"key2");
        let key3 = hash(b"key3");

        cache.insert(key1, module.clone());
        cache.insert(key2, module.clone());
        assert_eq!(cache.len(), 2);

        // Inserting a third entry should evict the LRU (key1).
        cache.insert(key3, module);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&key1).is_none(), "key1 should have been evicted");
        assert!(cache.get(&key2).is_some());
        assert!(cache.get(&key3).is_some());
    }
}
