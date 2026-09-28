//! Signed index entries (#1134 M3).
//!
//! Objects need no signature: their IDs are recomputed from content. An index
//! entry (cache key -> value) is the only thing a writer can lie about, so the
//! trusted writer signs each one and every reader checks the signature before
//! using it. The signature travels inside the value, as one reserved entry of
//! Xcode's `Value.entries` map, so the protocol is unchanged: a node strips it
//! before answering Xcode.
//!
//! Signed message: a domain tag, the cache key, then every other entry sorted
//! by name, each length-prefixed. Reserved entry: the signer's 32-byte public
//! key followed by the 64-byte Ed25519 signature.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::kv;

pub const SIG_ENTRY: &str = "glaeda.fleet-cas.sig.v1";
const DOMAIN: &[u8] = b"glaeda-fleet-cas-kv-v1\0";

fn push(m: &mut Vec<u8>, b: &[u8]) {
    m.extend((b.len() as u64).to_le_bytes());
    m.extend(b);
}

fn message(key: &[u8], value: &kv::Value) -> Vec<u8> {
    let mut names: Vec<&String> = value.entries.keys().filter(|k| *k != SIG_ENTRY).collect();
    names.sort();
    let mut m = DOMAIN.to_vec();
    push(&mut m, key);
    m.extend((names.len() as u64).to_le_bytes());
    for n in names {
        push(&mut m, n.as_bytes());
        push(&mut m, &value.entries[n]);
    }
    m
}

/// Replace any signature entry with this key's signature.
pub fn sign(sk: &SigningKey, key: &[u8], value: &mut kv::Value) {
    value.entries.remove(SIG_ENTRY);
    let sig = sk.sign(&message(key, value));
    let mut entry = sk.verifying_key().to_bytes().to_vec();
    entry.extend(sig.to_bytes());
    value.entries.insert(SIG_ENTRY.into(), entry);
}

/// True if the value carries a valid signature by one of `trusted`.
pub fn verify(trusted: &[VerifyingKey], key: &[u8], value: &kv::Value) -> bool {
    let Some(entry) = value.entries.get(SIG_ENTRY) else {
        return false;
    };
    if entry.len() != 96 {
        return false;
    }
    let Some(vk) = trusted.iter().find(|k| k.as_bytes() == &entry[..32]) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(&entry[32..]) else {
        return false;
    };
    vk.verify_strict(&message(key, value), &sig).is_ok()
}

/// The value without its signature entry, as Xcode expects it.
pub fn strip(mut value: kv::Value) -> kv::Value {
    value.entries.remove(SIG_ENTRY);
    value
}

pub fn parse_public(hex_key: &str) -> Result<VerifyingKey, String> {
    let bytes: [u8; 32] = hex::decode(hex_key.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("not a 32-byte hex public key: {hex_key}"))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| format!("bad public key {hex_key}: {e}"))
}

pub fn parse_trusted(list: &str) -> Result<Vec<VerifyingKey>, String> {
    let keys: Vec<VerifyingKey> = list
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(parse_public)
        .collect::<Result<_, _>>()?;
    if keys.is_empty() {
        return Err("--trusted-keys needs at least one key".into());
    }
    Ok(keys)
}

/// Load a signing key file: 64 hex characters, readable by its owner only.
pub fn load_signing_key(path: &Path) -> Result<SigningKey, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "{}: readable by group or others; chmod 600 it",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let seed: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("{}: not a 32-byte hex key", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Create a new signing key file (mode 0600, never overwriting) and return
/// its public key.
pub fn keygen(path: &Path) -> Result<VerifyingKey, String> {
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut seed))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    let sk = SigningKey::from_bytes(&seed);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let written = writeln!(f, "{}", hex::encode(seed)).and_then(|_| f.sync_all());
    if let Err(e) = written {
        // A partial key must not block the retry (keygen never overwrites).
        let _ = std::fs::remove_file(path);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(sk.verifying_key())
}

/// The cache key of a per-commit marker. The prefix keeps it apart from
/// Xcode's keys, which are binary digests.
pub const MARKER_PREFIX: &[u8] = b"glaeda-fleet-cas-marker-v1\0";

pub fn marker_key(name: &str) -> Vec<u8> {
    let mut k = MARKER_PREFIX.to_vec();
    k.extend(name.as_bytes());
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value() -> kv::Value {
        let mut v = kv::Value::default();
        v.entries.insert("a".into(), vec![1, 2, 3]);
        v.entries.insert("b".into(), vec![0; 32]);
        v
    }

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    #[test]
    fn signed_value_verifies_and_strips() {
        let sk = key(1);
        let mut v = value();
        sign(&sk, b"k", &mut v);
        assert!(verify(&[sk.verifying_key()], b"k", &v));
        let stripped = strip(v);
        assert_eq!(stripped, value());
    }

    #[test]
    fn rejects_tampering_other_keys_and_unsigned() {
        let sk = key(1);
        let mut v = value();
        sign(&sk, b"k", &mut v);
        let trusted = [sk.verifying_key()];
        // another cache key
        assert!(!verify(&trusted, b"other", &v));
        // a changed entry
        let mut t = v.clone();
        t.entries.get_mut("a").unwrap()[0] ^= 1;
        assert!(!verify(&trusted, b"k", &t));
        // an added entry
        let mut t = v.clone();
        t.entries.insert("c".into(), vec![]);
        assert!(!verify(&trusted, b"k", &t));
        // a removed entry
        let mut t = v.clone();
        t.entries.remove("b");
        assert!(!verify(&trusted, b"k", &t));
        // signed by an untrusted key
        let mut t = value();
        sign(&key(2), b"k", &mut t);
        assert!(!verify(&trusted, b"k", &t));
        // unsigned, or a truncated signature
        assert!(!verify(&trusted, b"k", &value()));
        let mut t = v.clone();
        t.entries.get_mut(SIG_ENTRY).unwrap().pop();
        assert!(!verify(&trusted, b"k", &t));
    }

    #[test]
    fn entry_boundaries_are_part_of_the_message() {
        // {"a": [1,2,3]} and {"a\0...": ...} style splices must not collide.
        let sk = key(1);
        let mut v = kv::Value::default();
        v.entries.insert("ab".into(), vec![1]);
        sign(&sk, b"k", &mut v);
        let mut t = kv::Value::default();
        t.entries.insert("a".into(), b"b\x01".to_vec());
        t.entries.insert(SIG_ENTRY.into(), v.entries[SIG_ENTRY].clone());
        assert!(!verify(&[sk.verifying_key()], b"k", &t));
    }

    #[test]
    fn keygen_writes_private_file_once() {
        let dir = std::env::temp_dir().join(format!("fleet-cas-keygen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("writer.key");
        let _ = std::fs::remove_file(&path);
        let vk = keygen(&path).unwrap();
        assert_eq!(load_signing_key(&path).unwrap().verifying_key(), vk);
        assert!(keygen(&path).is_err(), "must not overwrite");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_signing_key(&path).is_err(), "must refuse a readable key");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
