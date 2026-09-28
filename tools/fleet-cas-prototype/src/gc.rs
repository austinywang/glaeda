//! Access-based garbage collection for a fleet store.
//!
//! The store bumps an index entry's mtime when a read uses it (at most once an
//! hour per entry, see `touch_if_stale`), so an entry's mtime is its last use.
//! `fleet-cas gc STORE --keep-days N` keeps every entry used within N days and
//! every object reachable from a kept entry, and deletes the rest. Objects
//! written or re-uploaded within the last day are always kept (a deduplicated
//! upload bumps the object's mtime too), so a fill in progress, whose objects
//! land before the entry that names them, keeps its objects. Marker reads do
//! not count as uses, so a marker expires N days after its fill and never
//! outlives the entries it vouches for.
//!
//! Which objects an entry names comes from `embedded_ids`, a heuristic over
//! Xcode's value format. A kept entry that cannot be read or decoded aborts the
//! run before anything is deleted, and the report counts kept entries that
//! name no stored object: check that with `--dry-run` on a real store first.
//! Races with a running store (an entry read while gc deletes it) degrade to
//! misses, never wrong answers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use prost::Message;

use crate::{cas, embedded_ids, kv};

const TOUCH_EVERY: Duration = Duration::from_secs(3600);
const NEW_OBJECT_GRACE: Duration = Duration::from_secs(24 * 3600);

/// Record a use of the entry at `path` by bumping its mtime, unless it was
/// bumped within the last hour (reads are hot; one write per hour per entry).
pub fn touch_if_stale(path: &Path) {
    let now = SystemTime::now();
    let stale = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map_or(false, |t| now.duration_since(t).unwrap_or_default() > TOUCH_EVERY);
    if stale {
        if let Ok(f) = std::fs::File::options().append(true).open(path) {
            let _ = f.set_modified(now);
        }
    }
}

#[derive(Default, Debug)]
pub struct Report {
    pub kv_kept: u64,
    pub kv_deleted: u64,
    pub cas_kept: u64,
    pub cas_deleted: u64,
    pub bytes_deleted: u64,
    /// Kept entries none of whose embedded IDs is a stored object.
    pub kv_no_roots: u64,
}

fn remove(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(shards) = std::fs::read_dir(dir) else {
        return out;
    };
    for shard in shards.flatten() {
        if let Ok(entries) = std::fs::read_dir(shard.path()) {
            for e in entries.flatten() {
                let p = e.path();
                // Skip temporaries of in-flight writes (name.tmp<pid>.<seq>).
                if p.extension().is_none() && p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn age(path: &Path, now: SystemTime) -> Duration {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map_or(Duration::ZERO, |t| now.duration_since(t).unwrap_or_default())
}

pub fn run(root: &Path, keep: Duration, dry_run: bool) -> std::io::Result<Report> {
    let now = SystemTime::now();
    let mut r = Report::default();
    let cas_dir = root.join("cas");
    let path_of = |id: &[u8]| {
        let h = hex::encode(id);
        cas_dir.join(&h[..2]).join(h)
    };
    // 1. Index entries: keep the recently used ones, collect the IDs they name.
    // Nothing is deleted until every kept entry has been read: a kept entry
    // whose objects gc cannot see would lose them.
    let mut roots: Vec<Vec<u8>> = Vec::new();
    let mut stale = Vec::new();
    for p in files(&root.join("kv")) {
        if age(&p, now) <= keep {
            r.kv_kept += 1;
            let bytes = std::fs::read(&p)?;
            let v = kv::Value::decode(bytes.as_slice()).map_err(|e| {
                std::io::Error::other(format!("{}: undecodable entry ({e}); nothing deleted", p.display()))
            })?;
            let ids = embedded_ids(&v);
            // Markers name no objects by design; only Xcode entries count here.
            let marker = v.entries.contains_key("commit") && v.entries.contains_key("xcode");
            if !marker && !ids.iter().any(|id| path_of(id).exists()) {
                r.kv_no_roots += 1;
            }
            roots.extend(ids);
        } else {
            stale.push(p);
        }
    }
    for p in stale {
        r.kv_deleted += 1;
        if !dry_run {
            remove(&p)?;
        }
    }
    // 2. Objects: everything reachable from kept entries is live.
    let mut live: HashSet<Vec<u8>> = HashSet::new();
    let mut queue = roots;
    while let Some(id) = queue.pop() {
        if id.len() != 32 || !live.insert(id.clone()) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path_of(&id)) else {
            continue;
        };
        if let Ok(obj) = cas::CasObject::decode(bytes.as_slice()) {
            queue.extend(obj.references.into_iter().map(|r| r.id));
        }
    }
    // 3. Delete unreachable objects older than the grace period.
    for p in files(&cas_dir) {
        let id = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| hex::decode(n).ok());
        let keep_it = id.is_some_and(|id| live.contains(&id)) || age(&p, now) <= NEW_OBJECT_GRACE;
        if keep_it {
            r.cas_kept += 1;
        } else {
            r.cas_deleted += 1;
            r.bytes_deleted += std::fs::metadata(&p).map_or(0, |m| m.len());
            if !dry_run {
                remove(&p)?;
            }
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(refs: Vec<Vec<u8>>, data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let refs: Vec<cas::CasDataId> = refs.into_iter().map(|id| cas::CasDataId { id }).collect();
        let id = crate::object_id(&refs, data);
        let o = crate::data_object(refs, data.to_vec());
        (id, o.encode_to_vec())
    }

    fn put(root: &Path, dir: &str, name: &str, bytes: &[u8], age_secs: u64) -> PathBuf {
        let p = root.join(dir).join(&name[..2]).join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options().append(true).open(&p).unwrap().set_modified(t).unwrap();
        p
    }

    #[test]
    fn keeps_recent_entries_and_their_closure() {
        let root = std::env::temp_dir().join(format!("fleet-cas-gc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let day = 24 * 3600;
        let (leaf, leaf_b) = obj(vec![], b"leaf");
        let (top, top_b) = obj(vec![leaf.clone()], b"top");
        let (old, old_b) = obj(vec![], b"old");
        let (fresh, fresh_b) = obj(vec![], b"fresh-unreferenced");
        let leaf_p = put(&root, "cas", &hex::encode(&leaf), &leaf_b, 30 * day);
        let top_p = put(&root, "cas", &hex::encode(&top), &top_b, 30 * day);
        let old_p = put(&root, "cas", &hex::encode(&old), &old_b, 30 * day);
        let fresh_p = put(&root, "cas", &hex::encode(&fresh), &fresh_b, 60);
        let mut v = kv::Value::default();
        v.entries.insert("out".into(), top.clone());
        let recent = put(&root, "kv", &"a".repeat(64), &v.encode_to_vec(), 2 * day);
        let mut w = kv::Value::default();
        w.entries.insert("out".into(), old.clone());
        let stale = put(&root, "kv", &"b".repeat(64), &w.encode_to_vec(), 20 * day);

        let dry = run(&root, Duration::from_secs(7 * day), true).unwrap();
        assert_eq!((dry.kv_deleted, dry.cas_deleted), (1, 1));
        assert!(stale.exists() && old_p.exists(), "dry run deletes nothing");

        let r = run(&root, Duration::from_secs(7 * day), false).unwrap();
        assert_eq!((r.kv_kept, r.kv_deleted, r.cas_kept, r.cas_deleted), (1, 1, 3, 1));
        assert!(recent.exists() && !stale.exists());
        assert!(top_p.exists() && leaf_p.exists(), "closure of a kept entry stays");
        assert!(!old_p.exists(), "object only a stale entry named goes");
        assert!(fresh_p.exists(), "new objects stay through the grace period");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
