//! One generation-guarded value cache. The SQL pools and the MongoDB clients both use it: a
//! value built from a saved connection's settings is dropped as soon as those settings change,
//! and a value built from settings read before the change is never cached.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Mutex, MutexGuard};

/// A cache key: the value belongs to one saved connection (`owner`), which is what an update or
/// delete invalidates.
pub trait Owner {
    fn owner(&self) -> &str;
}

impl Owner for String {
    fn owner(&self) -> &str {
        self
    }
}

impl Owner for (String, String) {
    fn owner(&self) -> &str {
        &self.0
    }
}

struct Inner<K, V> {
    /// generation, value, tick of the last use.
    slots: HashMap<K, (u64, V, u64)>,
    /// Bumped by `close`. A value built from settings read before the bump is never cached.
    generations: HashMap<String, u64>,
    tick: u64,
}

impl<K: Eq + Hash, V> Inner<K, V> {
    fn generation(&self, owner: &str) -> u64 {
        self.generations.get(owner).copied().unwrap_or(0)
    }
}

/// Values by key, all built at a generation of their owner's settings.
pub struct GenerationCache<K, V> {
    inner: Mutex<Inner<K, V>>,
    /// Values kept per owner; 0 keeps every value. The least recently used one makes room.
    max_per_owner: usize,
}

/// The value handed out by [`GenerationCache::insert`].
pub struct Cached<V> {
    pub value: V,
    /// The least recently used value of the owner that made room, when the cap evicted one.
    pub evicted: Option<V>,
}

impl<K: Owner + Eq + Hash + Clone, V: Clone> GenerationCache<K, V> {
    pub fn with_cap(max_per_owner: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                slots: HashMap::new(),
                generations: HashMap::new(),
                tick: 0,
            }),
            max_per_owner,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner<K, V>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn generation(&self, owner: &str) -> u64 {
        self.lock().generation(owner)
    }

    pub fn is_current(&self, owner: &str, generation: u64) -> bool {
        self.generation(owner) == generation
    }

    pub fn get(&self, key: &K, generation: u64) -> Option<V> {
        let mut inner = self.lock();
        if inner.generation(key.owner()) != generation {
            return None;
        }
        inner.tick += 1;
        let tick = inner.tick;
        let (_, value, used) = inner.slots.get_mut(key)?;
        *used = tick;
        Some(value.clone())
    }

    /// Caches `value` for `key`. `None` when the owner's settings changed since `generation`,
    /// so the caller must re-read them. An entry cached by a parallel caller is reused.
    pub fn insert(&self, key: K, generation: u64, value: V) -> Option<Cached<V>> {
        let mut inner = self.lock();
        let owner = key.owner().to_string();
        if inner.generation(&owner) != generation {
            return None;
        }
        inner.tick += 1;
        let tick = inner.tick;
        if let Some((_, cached, used)) = inner.slots.get_mut(&key) {
            let value = cached.clone();
            *used = tick;
            return Some(Cached {
                value,
                evicted: None,
            });
        }
        let evicted = self.evict(&mut inner, &owner);
        inner.tick += 1;
        let tick = inner.tick;
        inner.slots.insert(key, (generation, value.clone(), tick));
        Some(Cached { value, evicted })
    }

    fn evict(&self, inner: &mut Inner<K, V>, owner: &str) -> Option<V> {
        if self.max_per_owner == 0 {
            return None;
        }
        let mine = inner
            .slots
            .iter()
            .filter(|(k, _)| k.owner() == owner)
            .count();
        if mine < self.max_per_owner {
            return None;
        }
        let oldest = inner
            .slots
            .iter()
            .filter(|(k, _)| k.owner() == owner)
            .min_by_key(|(_, (_, _, used))| *used)
            .map(|(k, _)| k.clone())?;
        inner.slots.remove(&oldest).map(|(_, v, _)| v)
    }

    /// Removes every value of `owner` and bumps its generation, so values being built right now
    /// are not cached.
    pub fn close(&self, owner: &str) -> Vec<V> {
        let mut inner = self.lock();
        *inner.generations.entry(owner.to_string()).or_insert(0) += 1;
        let keys: Vec<K> = inner
            .slots
            .keys()
            .filter(|k| k.owner() == owner)
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|k| inner.slots.remove(&k).map(|(_, v, _)| v))
            .collect()
    }

    pub fn contains(&self, owner: &str) -> bool {
        self.lock().slots.keys().any(|k| k.owner() == owner)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().slots.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(owner: &str, part: &str) -> (String, String) {
        (owner.to_string(), part.to_string())
    }

    #[test]
    fn a_stale_generation_is_never_cached_or_returned() {
        let cache: GenerationCache<(String, String), u32> = GenerationCache::with_cap(0);
        let g = cache.generation("c1");
        assert!(cache.get(&key("c1", "a"), g).is_none());
        let cached = cache.insert(key("c1", "a"), g, 1).unwrap();
        assert_eq!(cached.value, 1);
        assert_eq!(cache.get(&key("c1", "a"), g), Some(1));

        cache.close("c1");
        assert!(cache.get(&key("c1", "a"), g).is_none());
        assert!(cache.insert(key("c1", "a"), g, 2).is_none());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.generation("c1"), 1);
        assert_eq!(cache.generation("other"), 0);
    }

    #[test]
    fn a_parallel_insert_is_reused() {
        let cache: GenerationCache<(String, String), u32> = GenerationCache::with_cap(0);
        let g = cache.generation("c1");
        cache.insert(key("c1", "a"), g, 1).unwrap();
        let cached = cache.insert(key("c1", "a"), g, 2).unwrap();
        assert_eq!(cached.value, 1, "the first value stays");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn the_cap_evicts_the_least_recently_used_value() {
        let cache: GenerationCache<(String, String), &'static str> = GenerationCache::with_cap(2);
        let g = cache.generation("c1");
        cache.insert(key("c1", "a"), g, "a").unwrap();
        cache.insert(key("c1", "b"), g, "b").unwrap();
        cache.get(&key("c1", "a"), g);
        let cached = cache.insert(key("c1", "c"), g, "c").unwrap();
        assert_eq!(cached.evicted, Some("b"));
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&key("c1", "b"), g).is_none());
        assert_eq!(cache.get(&key("c1", "a"), g), Some("a"));
    }

    #[test]
    fn keys_of_other_owners_do_not_take_the_cap() {
        let cache: GenerationCache<(String, String), u32> = GenerationCache::with_cap(1);
        let g = cache.generation("c1");
        cache.insert(key("c1", "a"), g, 1).unwrap();
        let cached = cache.insert(key("c2", "a"), cache.generation("c2"), 2).unwrap();
        assert!(cached.evicted.is_none());
        assert_eq!(cache.get(&key("c1", "a"), g), Some(1));
    }
}
