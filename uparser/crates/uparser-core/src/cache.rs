//! Content-hash cache layer (T-9.1), per ARCHITECTURE.md §15. Key =
//! `sha256(source bytes) + protocol + parameter fingerprint` (endpoint,
//! model, and any other flags that change the *result* for otherwise
//! identical input); value = the full `ParseResult`. Stored as one JSON
//! file per entry under `<base_dir>/<hash[..2]>/<hash>.json`, gated by a
//! TTL.
//!
//! **Scope note**: §15 also describes a "layered sub-key" cache for
//! Profiler intermediate artifacts (rasterized pages/thumbnails/
//! classification responses), reusable between `classify` and a later
//! `parse` call. Nothing in this codebase currently produces those as an
//! independently cacheable value — `profiler.rs`'s `profile_l2` computes
//! straight from raw PDF bytes via `liteparse::is_complex()` in one call,
//! with no separate rasterized-page artifact exposed to cache. Building
//! that layer now would be speculative caching of a shape nothing
//! consumes yet, so this module implements the concretely useful half
//! (full `ParseResult` caching, genuinely hit-testable end to end) and
//! defers the sub-key layering until a real intermediate artifact exists
//! to key on.

use crate::types::ParseResult;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Everything besides the source bytes that changes what `ParseResult` a
/// given input produces — folded into the cache key so a re-run with a
/// different endpoint/model never returns a stale hit.
#[derive(Debug, Clone, Default)]
pub struct ParamFingerprint {
    pub protocol: String,
    pub endpoint: Option<String>,
    pub model: Option<String>,
}

impl ParamFingerprint {
    fn canonical(&self) -> String {
        format!(
            "{}|{}|{}",
            self.protocol,
            self.endpoint.as_deref().unwrap_or(""),
            self.model.as_deref().unwrap_or("")
        )
    }
}

/// `sha256(source_bytes) + protocol + param fingerprint`, hex-encoded.
pub fn cache_key(source_bytes: &[u8], params: &ParamFingerprint) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source_bytes);
    hasher.update(b"\0");
    hasher.update(params.canonical().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    stored_at_unix_secs: u64,
    result: ParseResult,
}

fn entry_path(base_dir: &Path, key: &str) -> PathBuf {
    let (prefix, _) = key.split_at(key.len().min(2));
    base_dir.join(prefix).join(format!("{key}.json"))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The real cache directory: `$UPARSER_CACHE_DIR` if set (lets tests —
/// and cautious users — redirect it away from the real home directory
/// without threading a path through every call site), else
/// `$HOME/.cache/uparser`, else the system temp dir if `HOME` isn't set
/// either (e.g. some sandboxed CI runners). Not a dependency-bearing
/// `dirs`-crate lookup; this project already avoids adding dependencies
/// for something this simple.
pub fn default_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("UPARSER_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join(".cache").join("uparser"),
        _ => std::env::temp_dir().join("uparser-cache"),
    }
}

/// Look up `key` under `base_dir`, returning `Some(result)` only if a
/// fresh (within `ttl`) entry exists. Any read/parse failure (missing
/// file, corrupt JSON, stale entry) is treated as a clean miss, never an
/// error — a cache is an optimization, not a source of truth.
pub fn get(base_dir: &Path, key: &str, ttl: Duration) -> Option<ParseResult> {
    let path = entry_path(base_dir, key);
    let bytes = std::fs::read(&path).ok()?;
    let entry: CacheEntry = serde_json::from_slice(&bytes).ok()?;
    let age = now_unix_secs().saturating_sub(entry.stored_at_unix_secs);
    if age > ttl.as_secs() {
        return None;
    }
    Some(entry.result)
}

/// Write `result` under `key`, creating parent directories as needed.
pub fn put(base_dir: &Path, key: &str, result: &ParseResult) -> io::Result<()> {
    let path = entry_path(base_dir, key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let entry = CacheEntry {
        stored_at_unix_secs: now_unix_secs(),
        result: result.clone(),
    };
    let json = serde_json::to_vec(&entry)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(path, json)
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct CacheStats {
    pub entries: usize,
    pub total_bytes: u64,
}

/// Walk `base_dir` and report entry count / total size on disk. Returns
/// zeroed stats (not an error) if the cache directory doesn't exist yet.
pub fn stat(base_dir: &Path) -> io::Result<CacheStats> {
    let mut entries = 0usize;
    let mut total_bytes = 0u64;
    if !base_dir.exists() {
        return Ok(CacheStats {
            entries,
            total_bytes,
        });
    }
    for shard in std::fs::read_dir(base_dir)? {
        let shard = shard?;
        if !shard.file_type()?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(shard.path())? {
            let file = file?;
            if file.file_type()?.is_file() {
                entries += 1;
                total_bytes += file.metadata()?.len();
            }
        }
    }
    Ok(CacheStats {
        entries,
        total_bytes,
    })
}

/// Remove the entire cache directory. A missing directory is a no-op
/// success, not an error.
pub fn clear(base_dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(base_dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RoutedBy;
    use std::collections::HashMap;

    fn sample_result(protocol: &str) -> ParseResult {
        ParseResult {
            source_path: "doc.pdf".into(),
            source_sha256: "abc".into(),
            protocol: protocol.into(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            model_endpoint: None,
            model_name: None,
            pages: vec![],
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: HashMap::new(),
        }
    }

    #[test]
    fn cache_key_is_stable_for_identical_input() {
        let params = ParamFingerprint {
            protocol: "mock".into(),
            endpoint: None,
            model: None,
        };
        assert_eq!(cache_key(b"hello", &params), cache_key(b"hello", &params));
    }

    #[test]
    fn cache_key_differs_by_source_bytes() {
        let params = ParamFingerprint {
            protocol: "mock".into(),
            ..Default::default()
        };
        assert_ne!(cache_key(b"a", &params), cache_key(b"b", &params));
    }

    #[test]
    fn cache_key_differs_by_protocol() {
        let a = ParamFingerprint {
            protocol: "mineru-vlm".into(),
            ..Default::default()
        };
        let b = ParamFingerprint {
            protocol: "dots-ocr".into(),
            ..Default::default()
        };
        assert_ne!(cache_key(b"same bytes", &a), cache_key(b"same bytes", &b));
    }

    #[test]
    fn cache_key_differs_by_endpoint_override() {
        let a = ParamFingerprint {
            protocol: "mineru-vlm".into(),
            endpoint: Some("http://a".into()),
            model: None,
        };
        let b = ParamFingerprint {
            protocol: "mineru-vlm".into(),
            endpoint: Some("http://b".into()),
            model: None,
        };
        assert_ne!(cache_key(b"same bytes", &a), cache_key(b"same bytes", &b));
    }

    #[test]
    fn put_then_get_round_trips_within_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let key = "deadbeef";
        let result = sample_result("mock");

        put(dir.path(), key, &result).unwrap();
        let hit = get(dir.path(), key, Duration::from_secs(3600));
        assert_eq!(hit, Some(result));
    }

    #[test]
    fn get_misses_cleanly_for_unknown_key() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(get(dir.path(), "nope", Duration::from_secs(3600)), None);
    }

    #[test]
    fn get_misses_when_entry_is_older_than_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let key = "deadbeef";
        let path = entry_path(dir.path(), key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let stale_entry = CacheEntry {
            stored_at_unix_secs: 0, // 1970 — always "older than any TTL"
            result: sample_result("mock"),
        };
        std::fs::write(&path, serde_json::to_vec(&stale_entry).unwrap()).unwrap();

        assert_eq!(get(dir.path(), key, Duration::from_secs(60)), None);
    }

    #[test]
    fn get_ignores_corrupt_json_and_misses_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let key = "deadbeef";
        let path = entry_path(dir.path(), key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();

        assert_eq!(get(dir.path(), key, Duration::from_secs(3600)), None);
    }

    #[test]
    fn stat_counts_entries_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "key1", &sample_result("mock")).unwrap();
        put(dir.path(), "key2", &sample_result("mineru-vlm")).unwrap();

        let stats = stat(dir.path()).unwrap();
        assert_eq!(stats.entries, 2);
        assert!(stats.total_bytes > 0);
    }

    #[test]
    fn stat_on_missing_dir_is_zeroed_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let stats = stat(&missing).unwrap();
        assert_eq!(
            stats,
            CacheStats {
                entries: 0,
                total_bytes: 0
            }
        );
    }

    #[test]
    fn clear_removes_all_entries() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "key1", &sample_result("mock")).unwrap();
        clear(dir.path()).unwrap();
        assert_eq!(stat(dir.path()).unwrap().entries, 0);
    }

    #[test]
    fn clear_on_missing_dir_is_a_no_op_success() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(clear(&missing).is_ok());
    }
}
