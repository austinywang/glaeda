//! `fleet-cas warm`: fill a node store with a whole commit before a build.
//!
//! A node answers each of Xcode's index lookups it lacks with a fetch from
//! the fleet store, then a closure prefetch, before Xcode can go on. Xcode
//! issues those lookups mostly one at a time, so a catch-up build that reads
//! everything remotely costs thousands of serial round trips (the 09-25 pair
//! test: 7837 lookups, about 336 ms each under load, 1631 s). The writer knows
//! every key its fill used: `fleet-cas-writer-build.sh` stores that list as a
//! CAS object (the manifest) and names it in the signed marker. `warm` reads
//! the marker, fetches the listed entries with many calls in flight, streams
//! the objects they reach in a few closure calls, and writes both into the
//! node's store, so the build's lookups are local hits.
//!
//! Nothing is trusted that the node would not trust: the marker and every
//! entry must carry a trusted signature, the manifest and every object are
//! checked against their content IDs.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use prost::Message;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::{
    MANIFEST_ENTRY, MAX_CLOSURE_ROOTS, Stats, Store, USAGE, blob_data, cas, embedded_ids, fleet,
    kv, object_id, publish_new, sign, write_atomic,
};

/// The manifest: one hex index key per line. Refuses an empty or malformed list.
pub fn parse_manifest(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let text = std::str::from_utf8(data).map_err(|_| "manifest is not text")?;
    let mut keys = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        keys.push(hex::decode(line).map_err(|_| format!("manifest line is not hex: {line:.40}"))?);
    }
    if keys.is_empty() {
        return Err("manifest lists no keys".into());
    }
    Ok(keys)
}

enum Entry {
    Local,
    /// Fetched and verified, not written yet: (path, encoded value, replace
    /// an unservable local copy, IDs it names).
    Fetched(PathBuf, Vec<u8>, bool, Vec<Vec<u8>>),
    Missing,
    Unverified,
}

/// Why a warm did not finish. Exit codes: 2 for `Untrusted`, 3 for `Incomplete`.
enum Failure {
    /// Something did not verify: a marker, an entry, the manifest or an object.
    Untrusted(String),
    /// The store was unreachable, busy, slow or failed a call. Nothing that
    /// would slow the build down was written (see the order in `warm`).
    Incomplete(String),
}

impl<E: std::fmt::Display> From<E> for Failure {
    fn from(e: E) -> Self {
        Failure::Incomplete(e.to_string())
    }
}

pub async fn run(args: &[String]) -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    let (Some(url), Some(name)) = (args.first(), args.get(1)) else {
        return Err(USAGE.into());
    };
    let mut trusted = None;
    let mut dir = None;
    let mut jobs = 32usize;
    let mut timeout = 150u64;
    let mut rest = args[2..].iter();
    while let Some(a) = rest.next() {
        let v = rest.next().ok_or(USAGE)?;
        match a.as_str() {
            "--trusted-keys" => trusted = Some(sign::parse_trusted(v)?),
            "--store" => dir = Some(PathBuf::from(v)),
            "--jobs" => jobs = v.parse::<usize>()?.clamp(1, 256),
            "--timeout" => timeout = v.parse::<u64>()?.max(1),
            _ => return Err(USAGE.into()),
        }
    }
    let keys = trusted.ok_or("warm needs --trusted-keys")?;
    let dir = dir.ok_or("warm needs --store DIR (the node's store)")?;
    let t0 = Instant::now();
    // One deadline for the whole warm: a stalled stream or a store that
    // accepts connections and never answers must not hold the build up.
    let limit = std::time::Duration::from_secs(timeout);
    let result = match tokio::time::timeout(limit, warm(url, name, keys, dir, jobs)).await {
        Ok(r) => r,
        Err(_) => Err(Failure::Incomplete(format!("not done after {timeout} s"))),
    };
    match result {
        Ok(Some(line)) => {
            println!("warm {name}: {line} in {:.1} s", t0.elapsed().as_secs_f64());
            Ok(std::process::ExitCode::SUCCESS)
        }
        Ok(None) => Ok(std::process::ExitCode::from(1)),
        Err(Failure::Untrusted(e)) => {
            eprintln!("fleet-cas warm: {e}");
            Ok(std::process::ExitCode::from(2))
        }
        Err(Failure::Incomplete(e)) => {
            eprintln!("fleet-cas warm: incomplete after {:.1} s: {e}", t0.elapsed().as_secs_f64());
            Ok(std::process::ExitCode::from(3))
        }
    }
}

/// Upper bound on what one warm writes: a full cmux fill is about 1.2 GB, and
/// a store (or anyone on the plain-HTTP path) could stream objects nobody asked for.
const MAX_WARM_BYTES: u64 = 16 << 30;

/// Warm in an order that never leaves the node slower than no warm: entries
/// are fetched into memory, then everything they reach is stored, and only
/// then are the entries written. A node answers a local entry without a
/// prefetch, so an entry written ahead of its objects would cost one fetch per
/// object; an entry left out still gets the node's fetch plus closure prefetch.
async fn warm(
    url: &str,
    name: &str,
    keys: Vec<ed25519_dalek::VerifyingKey>,
    dir: PathBuf,
    jobs: usize,
) -> Result<Option<String>, Failure> {
    let ch = tonic::transport::Endpoint::from_shared(url.to_string())?
        .tcp_nodelay(true)
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(30))
        .http2_keep_alive_interval(std::time::Duration::from_secs(10))
        .keep_alive_timeout(std::time::Duration::from_secs(5))
        .keep_alive_while_idle(true)
        .connect()
        .await?;
    let kv_client = kv::key_value_db_client::KeyValueDbClient::new(ch.clone());
    let mut cas_client = cas::casdb_service_client::CasdbServiceClient::new(ch.clone())
        .max_decoding_message_size(512 << 20);
    let fleet_client =
        fleet::fleet_cas_client::FleetCasClient::new(ch).max_decoding_message_size(512 << 20);

    // 1. The marker, and the manifest it names.
    let mkey = sign::marker_key(name);
    let resp = kv_client
        .clone()
        .get_value(kv::GetValueRequest { key: mkey.clone() })
        .await?;
    let Some(kv::get_value_response::Contents::Value(marker)) = resp.into_inner().contents else {
        eprintln!("fleet-cas warm: no marker {name}");
        return Ok(None);
    };
    if !sign::verify(&keys, &mkey, &marker) {
        return Err(Failure::Untrusted(format!("marker {name} is not signed by a trusted key")));
    }
    let Some(mid) = marker.entries.get(MANIFEST_ENTRY).cloned() else {
        eprintln!("fleet-cas warm: marker {name} has no manifest (written before warm existed)");
        return Ok(None);
    };
    let resp = cas_client
        .get(cas::CasGetRequest {
            cas_id: Some(cas::CasDataId { id: mid.clone() }),
            write_to_disk: false,
        })
        .await?
        .into_inner();
    let Some(cas::cas_get_response::Contents::Data(obj)) = resp.contents else {
        return Err(Failure::Incomplete(format!("the store lacks manifest {}", hex::encode(&mid))));
    };
    if object_id(&obj.references, blob_data(&obj)) != mid {
        return Err(Failure::Untrusted("the manifest does not match its ID".into()));
    }
    let wanted = parse_manifest(blob_data(&obj)).map_err(Failure::Untrusted)?;

    // The node's store, written the way the node writes it.
    std::fs::create_dir_all(&dir)?;
    let store = Arc::new(Store {
        root: dir,
        read_only_kv: true,
        writers: None,
        allow_file_paths: false,
        upstream: None,
        sign_key: None,
        trusted: Some(keys),
        strip_signatures: true,
        upstream_down_until: AtomicU64::new(0),
        stats: Stats::default(),
        key_log: None,
    });

    // 2. Index entries, many in flight, kept in memory. The first failed call
    // stops the warm: a store that fails one call is likely failing them all.
    let slots = Arc::new(Semaphore::new(jobs));
    // Dropping the set (an early return) aborts the calls still in flight.
    let mut set = JoinSet::new();
    let mut done = Vec::with_capacity(wanted.len());
    for key in wanted.iter().cloned() {
        let store = store.clone();
        let mut client = kv_client.clone();
        let permit = slots.clone().acquire_owned().await?;
        set.spawn(async move {
            let _permit = permit;
            let path = store.kv_path(&key);
            let mut replace = false;
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(v) = kv::Value::decode(bytes.as_slice()) {
                    if store.trusts(&key, &v) {
                        // A use, so the node store's gc keeps it (fleet-cas-prewarm.sh).
                        crate::gc::touch_if_stale(&path);
                        return Ok(Entry::Local);
                    }
                }
                replace = true;
            }
            let resp = client
                .get_value(kv::GetValueRequest { key: key.clone() })
                .await
                .map_err(|e| e.to_string())?;
            let Some(kv::get_value_response::Contents::Value(v)) = resp.into_inner().contents else {
                return Ok(Entry::Missing);
            };
            if !store.trusts(&key, &v) {
                return Ok(Entry::Unverified);
            }
            let ids = embedded_ids(&v);
            Ok::<_, String>(Entry::Fetched(path, v.encode_to_vec(), replace, ids))
        });
        // Collect as we go, so a failure stops new calls early.
        while let Some(r) = set.try_join_next() {
            done.push(r??);
        }
    }
    while let Some(r) = set.join_next().await {
        done.push(r??);
    }
    let (mut local, mut missing, mut unverified) = (0, 0, 0);
    let mut fetched = Vec::new();
    for entry in done {
        match entry {
            Entry::Local => local += 1,
            Entry::Fetched(path, bytes, replace, ids) => fetched.push((path, bytes, replace, ids)),
            Entry::Missing => missing += 1,
            Entry::Unverified => unverified += 1,
        }
    }
    if unverified > 0 {
        return Err(Failure::Untrusted(format!("{unverified} entries without a trusted signature")));
    }

    // 3. Everything the fetched entries reach, in closure streams. Roots are not
    // filtered by what is on disk: an object can be present without its
    // references (an earlier interrupted prefetch), and the stream fills them in.
    let mut roots: Vec<Vec<u8>> = fetched.iter().flat_map(|f| f.3.iter().cloned()).collect();
    roots.sort();
    roots.dedup();
    let objects = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    // Two streams per warm: the store serves 64 walks at once, shared with
    // every node's prefetches and the other hosts' warms.
    let streams = Arc::new(Semaphore::new(2));
    let mut set = JoinSet::new();
    for chunk in roots.chunks(MAX_CLOSURE_ROOTS) {
        let chunk = chunk.to_vec();
        let store = store.clone();
        let mut client = fleet_client.clone();
        let (objects, bytes) = (objects.clone(), bytes.clone());
        let permit = streams.clone().acquire_owned().await?;
        set.spawn(async move {
            let _permit = permit;
            // A busy store is retried with backoff, within the warm's deadline.
            let mut wait = std::time::Duration::from_millis(500);
            let mut stream = loop {
                match client
                    .get_closure(fleet::ClosureRequest { roots: chunk.clone() })
                    .await
                {
                    Ok(r) => break r.into_inner(),
                    Err(e) if e.code() == tonic::Code::ResourceExhausted => {
                        tokio::time::sleep(wait).await;
                        wait = (wait * 2).min(std::time::Duration::from_secs(8));
                    }
                    Err(e) => return Err(Failure::Incomplete(e.to_string())),
                }
            };
            while let Some(m) = stream.message().await? {
                let Ok(obj) = cas::CasObject::decode(m.object.as_slice()) else {
                    return Err(Failure::Untrusted("undecodable object in a closure".into()));
                };
                let data = blob_data(&obj).to_vec();
                if object_id(&obj.references, &data) != m.id {
                    return Err(Failure::Untrusted("closure object does not match its ID".into()));
                }
                if store.cas_path(&m.id).exists() {
                    continue;
                }
                let len = data.len() as u64;
                if bytes.fetch_add(len, Relaxed) + len > MAX_WARM_BYTES {
                    return Err(Failure::Untrusted(format!(
                        "the store sent more than {MAX_WARM_BYTES} bytes"
                    )));
                }
                store.put(obj.references, data).map_err(|e| e.message().to_string())?;
                objects.fetch_add(1, Relaxed);
            }
            Ok(())
        });
    }
    while let Some(r) = set.join_next().await {
        if let Err(e) = r? {
            set.abort_all();
            return Err(e);
        }
    }

    // 4. Only now the entries: their objects are in place.
    let written = fetched.len();
    for (path, value, replace, _) in fetched {
        std::fs::create_dir_all(path.parent().unwrap())?;
        if replace {
            write_atomic(&path, &value)?;
        } else {
            // Lost a race with the node: its copy is as good.
            publish_new(&path, &value)?;
        }
    }
    Ok(Some(format!(
        "{} keys ({local} local, {written} fetched, {missing} missing), {} objects ({} bytes)",
        wanted.len(),
        objects.load(Relaxed),
        bytes.load(Relaxed)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parsing() {
        assert_eq!(parse_manifest(b"00ff\nab\n").unwrap(), vec![vec![0, 255], vec![0xab]]);
        assert!(parse_manifest(b"").is_err());
        assert!(parse_manifest(b"\n\n").is_err());
        assert!(parse_manifest(b"zz\n").is_err());
    }
}
