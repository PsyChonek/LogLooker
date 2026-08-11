//! In-memory cache of the decompressed log text.
//!
//! It holds text, not compressed bytes. Caching the `.zst` files in RAM would
//! only duplicate the OS page cache, which already keeps them there for free -
//! measured, it is exactly as fast as reading them from disk.
//!
//! What the text buys is parallelism: a zstd stream can only be decoded from
//! the front, so a search over one file is stuck on one core. Text in RAM can be
//! split at line boundaries and scanned on every core. That, and opening the raw
//! viewer without decompressing to a temp file first, is why this exists.
//!
//! `max_bytes` is a hard cap. Files that do not fit are streamed from disk as
//! before, so a 10 GB selection searches fine under a 2 GB cap - it just does
//! not get the parallel scan.

use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

pub const DEFAULT_MAX_MB: u64 = 2048;
/// Below the minimum the cache cannot hold a day of most services. 0 means
/// unlimited; anything above the minimum is a custom cap taken as intended.
pub const MIN_MAX_MB: u64 = 64;

pub fn clamp_max_mb(max_mb: u64) -> u64 {
    match max_mb {
        0 => 0,
        mb => mb.max(MIN_MAX_MB),
    }
}

/// 0 (unlimited) becomes a cap the arithmetic can compare against; real byte
/// counts never approach it.
fn max_mb_to_bytes(max_mb: u64) -> u64 {
    match clamp_max_mb(max_mb) {
        0 => u64::MAX,
        mb => mb * 1024 * 1024,
    }
}

/// Identifies the exact bytes on disk. A live file (today's log, Log.txt) grows
/// between syncs and its `.zst` is rewritten or appended to - length and mtime
/// then differ from what was cached and the entry is reloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    len: u64,
    modified: Option<SystemTime>,
}

fn version_of(path: &Path) -> Option<Version> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Version {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

struct Entry {
    version: Version,
    text: Arc<[u8]>,
    last_used: u64,
    /// Search pass that last touched this entry - a pass never evicts what it
    /// has already loaded for itself
    last_pass: u64,
}

/// Where a reader takes one cached file from. The `Arc` keeps the text alive for
/// as long as the caller holds the source, even if another thread evicts it.
pub enum Source {
    Memory(Arc<[u8]>),
    Disk(PathBuf),
}

impl Source {
    /// Sequential reader over the decompressed text, whichever tier it comes from.
    pub fn reader(&self) -> Result<Box<dyn BufRead + '_>, String> {
        match self {
            Source::Memory(text) => Ok(Box::new(Cursor::new(text.as_ref()))),
            Source::Disk(path) => {
                let file = std::fs::File::open(path)
                    .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
                let decoder = zstd::stream::read::Decoder::new(file)
                    .map_err(|e| format!("zstd open failed for {}: {e}", path.display()))?;
                Ok(Box::new(BufReader::new(decoder)))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemCacheStats {
    pub enabled: bool,
    pub max_bytes: u64,
    pub used_bytes: u64,
    pub files: usize,
}

pub struct MemCache(Mutex<Inner>);

struct Inner {
    enabled: bool,
    max_bytes: u64,
    used_bytes: u64,
    /// Room taken by files being decompressed right now - they own their bytes
    /// before they have them, so a concurrent load cannot claim the same room
    reserved_bytes: u64,
    /// Monotonic tick for LRU order
    clock: u64,
    /// Bumped once per search
    pass: u64,
    entries: HashMap<PathBuf, Entry>,
}

impl MemCache {
    pub fn new(enabled: bool, max_mb: u64) -> Self {
        Self::with_max_bytes(enabled, max_mb_to_bytes(max_mb))
    }

    fn with_max_bytes(enabled: bool, max_bytes: u64) -> Self {
        MemCache(Mutex::new(Inner {
            enabled,
            max_bytes,
            used_bytes: 0,
            reserved_bytes: 0,
            clock: 0,
            pass: 0,
            entries: HashMap::new(),
        }))
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner>, String> {
        self.0
            .lock()
            .map_err(|_| "Memory cache lock is poisoned".to_string())
    }

    /// Turning the cache off, or shrinking it, frees the memory right away.
    pub fn configure(&self, enabled: bool, max_mb: u64) -> Result<(), String> {
        self.configure_bytes(enabled, max_mb_to_bytes(max_mb))
    }

    fn configure_bytes(&self, enabled: bool, max_bytes: u64) -> Result<(), String> {
        let mut inner = self.lock()?;
        inner.enabled = enabled;
        inner.max_bytes = max_bytes;
        if enabled {
            // A pass in flight must not keep its files pinned against the new cap
            inner.pass += 1;
            inner.make_room(0);
        } else {
            inner.drop_all();
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<(), String> {
        self.lock()?.drop_all();
        Ok(())
    }

    /// Drops the entry for a file about to be deleted from the disk cache.
    pub fn forget(&self, path: &Path) -> Result<(), String> {
        self.lock()?.remove(path);
        Ok(())
    }

    pub fn stats(&self) -> Result<MemCacheStats, String> {
        let inner = self.lock()?;
        Ok(MemCacheStats {
            enabled: inner.enabled,
            // 0 stands for unlimited; u64::MAX would lose precision as a JS number
            max_bytes: match inner.max_bytes {
                u64::MAX => 0,
                bytes => bytes,
            },
            used_bytes: inner.used_bytes,
            files: inner.entries.len(),
        })
    }

    /// Starts a search pass. Files this pass loads are pinned for its duration,
    /// so a selection larger than the budget streams the overflow instead of
    /// evicting its own files one after another.
    pub fn begin_pass(&self) {
        if let Ok(mut inner) = self.lock() {
            inner.pass += 1;
        }
    }

    /// The text of a cached file if it is already held - never loads it.
    /// Search uses this: loading a file costs a decompression that the scan
    /// would otherwise fold into its single streaming pass.
    pub fn get(&self, path: &Path) -> Result<Option<Arc<[u8]>>, String> {
        let Some(version) = version_of(path) else {
            return Ok(None);
        };
        let mut inner = self.lock()?;
        if !inner.enabled {
            return Ok(None);
        }
        Ok(inner.hit(path, version))
    }

    /// The text of a cached file, loading and holding it if it fits the budget.
    /// `None` means it does not fit (or the cache is off) and must be streamed.
    pub fn load(&self, path: &Path, uncompressed: u64) -> Result<Option<Arc<[u8]>>, String> {
        let Some(version) = version_of(path) else {
            return Ok(None);
        };
        {
            let mut inner = self.lock()?;
            if !inner.enabled {
                return Ok(None);
            }
            if let Some(text) = inner.hit(path, version) {
                return Ok(Some(text));
            }
            // The room is taken before the file is decompressed into it, so two
            // files loading at once cannot both pass the check and then both
            // allocate - which, at a gigabyte apiece, would overshoot the cap.
            if !inner.reserve(uncompressed) {
                return Ok(None);
            }
        }

        // Decompressed without the lock held, so parallel loads do not serialize
        let text = decompress(path, uncompressed);
        let mut inner = self.lock()?;
        inner.release(uncompressed);
        Ok(Some(inner.insert(path, version, Arc::from(text?))))
    }

    /// A reader for the file: from RAM when it is held, streamed from the zstd
    /// file otherwise. Never loads.
    pub fn source(&self, path: &Path) -> Result<Source, String> {
        match self.get(path)? {
            Some(text) => Ok(Source::Memory(text)),
            None => Ok(Source::Disk(path.to_path_buf())),
        }
    }
}

/// The manifest knows the log's size, so the buffer is allocated once - a `Vec`
/// that grows by doubling into gigabytes spends most of its time copying itself.
fn decompress(path: &Path, uncompressed: u64) -> Result<Vec<u8>, String> {
    let compressed =
        std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let mut text = Vec::with_capacity(uncompressed as usize);
    // Appended files hold several zstd frames; the decoder reads them all
    zstd::stream::copy_decode(compressed.as_slice(), &mut text)
        .map_err(|e| format!("zstd decompression failed for {}: {e}", path.display()))?;
    Ok(text)
}

impl Inner {
    fn hit(&mut self, path: &Path, version: Version) -> Option<Arc<[u8]>> {
        let stale = self
            .entries
            .get(path)
            .is_some_and(|entry| entry.version != version);
        if stale {
            self.remove(path);
            return None;
        }

        self.clock += 1;
        let (clock, pass) = (self.clock, self.pass);
        let entry = self.entries.get_mut(path)?;
        entry.last_used = clock;
        entry.last_pass = pass;
        Some(entry.text.clone())
    }

    /// Claims room for a file about to be decompressed. Fails when the file
    /// cannot fit even after evicting everything the running pass has not
    /// pinned - the caller then streams it instead.
    fn reserve(&mut self, bytes: u64) -> bool {
        let pinned: u64 = self
            .entries
            .values()
            .filter(|entry| entry.last_pass == self.pass)
            .map(|entry| entry.text.len() as u64)
            .sum();
        if bytes > self.max_bytes.saturating_sub(pinned) || !self.make_room(bytes) {
            return false;
        }
        self.reserved_bytes += bytes;
        true
    }

    fn release(&mut self, bytes: u64) {
        self.reserved_bytes = self.reserved_bytes.saturating_sub(bytes);
    }

    /// Evicts least-recently-used entries until `needed` bytes are free. Entries
    /// of the running pass are never evicted. Returns whether the room exists.
    fn make_room(&mut self, needed: u64) -> bool {
        if self.used_bytes + self.reserved_bytes + needed <= self.max_bytes {
            return true;
        }
        let mut candidates: Vec<(u64, PathBuf)> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.last_pass != self.pass)
            .map(|(path, entry)| (entry.last_used, path.clone()))
            .collect();
        candidates.sort_by_key(|(last_used, _)| *last_used);

        for (_, path) in &candidates {
            if self.used_bytes + self.reserved_bytes + needed <= self.max_bytes {
                return true;
            }
            self.remove(path);
        }
        self.used_bytes + self.reserved_bytes + needed <= self.max_bytes
    }

    /// Stores the text and hands back the shared copy. If it no longer fits (the
    /// manifest's size was optimistic, or another pass filled the cache while
    /// this one decompressed), the caller still gets the text - it is simply not
    /// kept, so the cap holds.
    fn insert(&mut self, path: &Path, version: Version, text: Arc<[u8]>) -> Arc<[u8]> {
        self.remove(path);
        let bytes = text.len() as u64;
        if !self.enabled || !self.make_room(bytes) {
            return text;
        }

        self.clock += 1;
        self.used_bytes += bytes;
        self.entries.insert(
            path.to_path_buf(),
            Entry {
                version,
                text: text.clone(),
                last_used: self.clock,
                last_pass: self.pass,
            },
        );
        text
    }

    fn remove(&mut self, path: &Path) {
        if let Some(entry) = self.entries.remove(path) {
            self.used_bytes -= entry.text.len() as u64;
        }
    }

    fn drop_all(&mut self) {
        self.entries.clear();
        self.used_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &[u8] = b"13.07.2026 00:00:02.611 INFO - Request starting HTTP/1.1 GET http://x\n";
    const LINES: usize = 1_000;

    fn text_bytes() -> u64 {
        (LINE.len() * LINES) as u64
    }

    /// Room for one file's text, never for two
    fn one_file_budget() -> u64 {
        text_bytes() + 1024
    }

    fn write_zst(dir: &Path, name: &str, lines: usize) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let raw = LINE.repeat(lines);
        std::fs::write(&path, zstd::stream::encode_all(raw.as_slice(), 3).unwrap()).unwrap();
        path
    }

    fn read_all(source: &Source) -> String {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut source.reader().unwrap(), &mut text).unwrap();
        text
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("loglooker-memcache").join(name);
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn a_disabled_cache_holds_nothing_and_reads_from_disk() {
        let dir = temp_dir("disabled");
        let path = write_zst(&dir, "a.log.zst", LINES);
        let cache = MemCache::with_max_bytes(false, one_file_budget());

        assert!(cache.load(&path, text_bytes()).unwrap().is_none());
        let source = cache.source(&path).unwrap();
        assert!(matches!(source, Source::Disk(_)));
        assert_eq!(read_all(&source).lines().count(), LINES);
        assert_eq!(cache.stats().unwrap().files, 0);
    }

    #[test]
    fn load_holds_the_text_and_get_serves_it_without_touching_the_disk() {
        let dir = temp_dir("load");
        let path = write_zst(&dir, "a.log.zst", LINES);
        let cache = MemCache::with_max_bytes(true, one_file_budget());

        cache.begin_pass();
        // Nothing is loaded until something asks for it to be
        assert!(cache.get(&path).unwrap().is_none());
        assert!(matches!(cache.source(&path).unwrap(), Source::Disk(_)));

        let text = cache.load(&path, text_bytes()).unwrap().unwrap();
        assert_eq!(text.len() as u64, text_bytes());

        cache.begin_pass();
        assert!(cache.get(&path).unwrap().is_some());
        let source = cache.source(&path).unwrap();
        assert!(matches!(source, Source::Memory(_)));
        assert_eq!(read_all(&source).lines().count(), LINES);

        let stats = cache.stats().unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.used_bytes, text_bytes());
    }

    #[test]
    fn a_rewritten_file_invalidates_the_entry() {
        let dir = temp_dir("stale");
        let path = write_zst(&dir, "a.log.zst", LINES);
        let cache = MemCache::with_max_bytes(true, one_file_budget());

        cache.begin_pass();
        cache.load(&path, text_bytes()).unwrap().unwrap();

        // A live file grew: same path, new bytes
        write_zst(&dir, "a.log.zst", LINES / 2);
        cache.begin_pass();
        assert!(
            cache.get(&path).unwrap().is_none(),
            "the stale copy is dropped, not served"
        );
        assert_eq!(cache.stats().unwrap().files, 0);

        let text = cache.load(&path, text_bytes() / 2).unwrap().unwrap();
        assert_eq!(text.len() as u64, text_bytes() / 2, "the new content");
    }

    #[test]
    fn a_file_too_big_for_the_budget_is_not_held() {
        let dir = temp_dir("toobig");
        let path = write_zst(&dir, "big.log.zst", LINES);
        let cache = MemCache::with_max_bytes(true, text_bytes() / 2);

        cache.begin_pass();
        assert!(cache.load(&path, text_bytes()).unwrap().is_none());
        assert_eq!(cache.stats().unwrap().files, 0);

        // It still searches, straight from the zstd file
        let source = cache.source(&path).unwrap();
        assert!(matches!(source, Source::Disk(_)));
        assert_eq!(read_all(&source).lines().count(), LINES);
    }

    #[test]
    fn the_least_recently_used_file_is_evicted_to_make_room() {
        let dir = temp_dir("evict");
        let old = write_zst(&dir, "old.log.zst", LINES);
        let new = write_zst(&dir, "new.log.zst", LINES);
        let cache = MemCache::with_max_bytes(true, one_file_budget());

        cache.begin_pass();
        cache.load(&old, text_bytes()).unwrap().unwrap();

        cache.begin_pass();
        cache.load(&new, text_bytes()).unwrap().unwrap();

        let stats = cache.stats().unwrap();
        assert_eq!(stats.files, 1, "only one text fits");
        assert!(stats.used_bytes <= stats.max_bytes);
        assert!(cache.get(&new).unwrap().is_some());
        assert!(cache.get(&old).unwrap().is_none(), "the older one gave way");
    }

    #[test]
    fn one_pass_does_not_evict_the_file_it_just_loaded() {
        let dir = temp_dir("pin");
        let first = write_zst(&dir, "first.log.zst", LINES);
        let second = write_zst(&dir, "second.log.zst", LINES);
        let cache = MemCache::with_max_bytes(true, one_file_budget());

        // Both files in one search: the first keeps its place, the second is
        // simply not held rather than throwing the first out
        cache.begin_pass();
        assert!(cache.load(&first, text_bytes()).unwrap().is_some());
        assert!(cache.load(&second, text_bytes()).unwrap().is_none());

        assert!(cache.get(&first).unwrap().is_some());
        assert!(cache.stats().unwrap().used_bytes <= one_file_budget());
    }

    #[test]
    fn shrinking_or_disabling_frees_the_memory_at_once() {
        let dir = temp_dir("shrink");
        let path = write_zst(&dir, "a.log.zst", LINES);
        let cache = MemCache::with_max_bytes(true, one_file_budget());

        cache.begin_pass();
        cache.load(&path, text_bytes()).unwrap().unwrap();
        assert_eq!(cache.stats().unwrap().used_bytes, text_bytes());

        cache.configure_bytes(true, text_bytes() / 2).unwrap();
        let stats = cache.stats().unwrap();
        assert_eq!(stats.files, 0, "the text no longer fits");
        assert_eq!(stats.used_bytes, 0);

        cache.begin_pass();
        cache.configure_bytes(true, one_file_budget()).unwrap();
        cache.load(&path, text_bytes()).unwrap().unwrap();
        cache.configure_bytes(false, one_file_budget()).unwrap();
        assert_eq!(cache.stats().unwrap().used_bytes, 0, "disabling frees all");
    }

    #[test]
    fn clearing_and_forgetting_free_their_entries() {
        let dir = temp_dir("clear");
        let path = write_zst(&dir, "a.log.zst", LINES);
        let cache = MemCache::with_max_bytes(true, one_file_budget());

        cache.begin_pass();
        cache.load(&path, text_bytes()).unwrap();
        cache.forget(&path).unwrap();
        assert_eq!(cache.stats().unwrap().used_bytes, 0);

        cache.load(&path, text_bytes()).unwrap();
        cache.clear().unwrap();
        assert_eq!(cache.stats().unwrap().files, 0);
    }

    #[test]
    fn a_hand_edited_budget_is_clamped_into_range() {
        assert_eq!(clamp_max_mb(0), 0);
        assert_eq!(clamp_max_mb(1), MIN_MAX_MB);
        assert_eq!(clamp_max_mb(1_000_000), 1_000_000);
        assert_eq!(clamp_max_mb(DEFAULT_MAX_MB), DEFAULT_MAX_MB);
        assert_eq!(max_mb_to_bytes(0), u64::MAX);
        assert_eq!(max_mb_to_bytes(DEFAULT_MAX_MB), DEFAULT_MAX_MB * 1024 * 1024);
    }
}
