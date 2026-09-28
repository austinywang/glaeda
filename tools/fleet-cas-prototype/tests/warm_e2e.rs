//! End to end: a signing writer node fills a store, the marker names the
//! fill's manifest (from the writer's key log), and `warm` copies the fill
//! into an empty reader store.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use hyper_util::rt::TokioIo;
use tonic::transport::Endpoint;

pub mod cas {
    tonic::include_proto!("compilation_cache_service.cas.v1");
}
pub mod kv {
    tonic::include_proto!("compilation_cache_service.keyvalue.v1");
}

const BIN: &str = env!("CARGO_BIN_EXE_fleet-cas");

struct Kill(Vec<Child>);
impl Drop for Kill {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(BIN).args(args).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

fn count_files(dir: &Path) -> usize {
    let Ok(shards) = std::fs::read_dir(dir) else { return 0 };
    shards
        .flatten()
        .map(|s| std::fs::read_dir(s.path()).map_or(0, |e| e.count()))
        .sum()
}

fn wait_for(what: &str, ok: impl Fn() -> bool) {
    let t = Instant::now();
    while !ok() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn writer_manifest_then_warm() {
    let root: PathBuf = std::env::temp_dir().join(format!("fleet-cas-warm-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let p = |n: &str| root.join(n).to_str().unwrap().to_string();

    let (rc, pubkey, err) = run(&["keygen", &p("writer.key")]);
    assert_eq!(rc, 0, "{err}");
    let pubkey = pubkey.trim().to_string();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");
    let store = Command::new(BIN)
        .args([&format!("tcp:127.0.0.1:{port}"), &p("store"), "--writers", "127.0.0.1", "--trusted-keys", &pubkey])
        .spawn()
        .unwrap();
    let node = Command::new(BIN)
        .args([&p("w.sock"), &p("wnode"), "--sign-key", &p("writer.key"), "--trusted-keys", &pubkey, "--upstream", &url])
        .spawn()
        .unwrap();
    let _guard = Kill(vec![store, node]);
    wait_for("store", || std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
    let sock = PathBuf::from(p("w.sock"));
    wait_for("node socket", || sock.exists());

    let ch = Endpoint::try_from("http://node")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            let sock = sock.clone();
            async move { Ok::<_, std::io::Error>(TokioIo::new(tokio::net::UnixStream::connect(sock).await?)) }
        }))
        .await
        .unwrap();
    let mut c = cas::casdb_service_client::CasdbServiceClient::new(ch.clone());
    let mut k = kv::key_value_db_client::KeyValueDbClient::new(ch);
    let put = |refs: Vec<Vec<u8>>, data: &[u8]| cas::CasPutRequest {
        data: Some(cas::CasObject {
            blob: Some(cas::CasBytes { contents: Some(cas::cas_bytes::Contents::Data(data.to_vec())) }),
            references: refs.into_iter().map(|id| cas::CasDataId { id }).collect(),
        }),
    };
    let id = |r: cas::CasPutResponse| match r.contents {
        Some(cas::cas_put_response::Contents::CasId(c)) => c.id,
        other => panic!("put failed: {other:?}"),
    };
    let leaf = id(c.put(put(vec![], b"leaf")).await.unwrap().into_inner());
    let top = id(c.put(put(vec![leaf.clone()], b"top")).await.unwrap().into_inner());
    for (key, out) in [(b"key-one".to_vec(), top), (b"key-two".to_vec(), leaf)] {
        let mut v = kv::Value::default();
        v.entries.insert("out".into(), out);
        let r = k.put_value(kv::PutValueRequest { key, value: Some(v) }).await.unwrap().into_inner();
        assert!(r.error.is_none(), "{:?}", r.error);
    }
    // A lookup the build hits is part of the fill too.
    k.get_value(kv::GetValueRequest { key: b"key-one".to_vec() }).await.unwrap();

    // What fleet-cas-writer-build.sh does after the build.
    let log = std::fs::read_to_string(root.join("wnode/fill-keys.log")).unwrap();
    let mut lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3, "two puts and one hit: {log}");
    lines.sort();
    lines.dedup();
    std::fs::write(root.join("manifest"), lines.join("\n") + "\n").unwrap();
    let (rc, _, err) = run(&["marker", "put", &url, "r/c/x", "--sign-key", &p("writer.key"), "--entry", "repo=r", "--manifest", &p("manifest")]);
    assert_eq!(rc, 0, "{err}");
    let (rc, out, err) = run(&["marker", "get", &url, "r/c/x", "--trusted-keys", &pubkey]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("manifest=") && out.contains("repo=r"), "{out}");

    let (rc, out, err) = run(&["warm", &url, "r/c/x", "--trusted-keys", &pubkey, "--store", &p("reader")]);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(out.contains("2 keys (0 local, 2 fetched, 0 missing), 2 objects"), "{out}");
    assert_eq!(count_files(&root.join("reader/kv")), 2);
    assert_eq!(count_files(&root.join("reader/cas")), 2);
    // Again: everything is local now.
    let (rc, out, _) = run(&["warm", &url, "r/c/x", "--trusted-keys", &pubkey, "--store", &p("reader")]);
    assert_eq!(rc, 0);
    assert!(out.contains("(2 local, 0 fetched") && out.contains(" 0 objects"), "{out}");

    // An untrusted key: nothing is copied from the marker it cannot verify.
    let other = "00".repeat(32);
    let (rc, _, _) = run(&["warm", &url, "r/c/x", "--trusted-keys", &other, "--store", &p("reader2")]);
    assert_ne!(rc, 0);
    assert_eq!(count_files(&root.join("reader2/kv")), 0);
    // A store that is gone: incomplete (3), nothing written.
    let dead = format!("http://127.0.0.1:{}", std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port());
    let (rc, _, _) = run(&["warm", &dead, "r/c/x", "--trusted-keys", &pubkey, "--store", &p("reader3")]);
    assert_eq!(rc, 3);
    assert_eq!(count_files(&root.join("reader3/kv")), 0);
    // No marker: exit 1.
    let (rc, _, _) = run(&["warm", &url, "r/c/none", "--trusted-keys", &pubkey, "--store", &p("reader2")]);
    assert_eq!(rc, 1);

    // Prewarm (needs Xcode for the build version, as on the minis): the latest marker
    // names a commit, and prewarm warms it into the node store.
    let xcode = Command::new("xcodebuild").arg("-version").output().ok().and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("Build version ").map(str::to_string))
    });
    if let Some(xcode) = xcode {
        let commit = "ab".repeat(20);
        let (rc, _, err) = run(&["marker", "put", &url, &format!("r/{commit}/{xcode}"), "--sign-key", &p("writer.key"), "--entry", "commit=x", "--manifest", &p("manifest")]);
        assert_eq!(rc, 0, "{err}");
        // The latest marker moves: a newer signed put replaces it.
        for c in ["cd".repeat(20), commit.clone()] {
            let (rc, _, err) = run(&["marker", "put", &url, &format!("r/latest/{xcode}"), "--sign-key", &p("writer.key"), "--entry", &format!("commit={c}")]);
            assert_eq!(rc, 0, "{err}");
        }
        let (_, out, _) = run(&["marker", "get", &url, &format!("r/latest/{xcode}"), "--trusted-keys", &pubkey]);
        assert!(out.contains(&format!("commit={commit}")), "{out}");
        let pw = root.join("pw");
        std::fs::create_dir_all(pw.join("bin")).unwrap();
        std::os::unix::fs::symlink(BIN, pw.join("bin/fleet-cas")).unwrap();
        std::fs::write(pw.join("fleet-cas.env"), format!("FLEET_CAS_STORE=127.0.0.1:{port}\nFLEET_CAS_TRUSTED_KEYS={pubkey}\nFLEET_CAS_SIGN_KEY=\n")).unwrap();
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/fleet-cas-prewarm.sh");
        let prewarm = || {
            let o = Command::new(script)
                .arg("r")
                .env("FLEET_CAS_ROOT", &pw)
                .env("FLEET_HOST_LOCK", root.join("no-such-lock"))
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).to_string()
        };
        let out = prewarm();
        assert!(out.contains("2 fetched"), "{out}");
        assert_eq!(count_files(&pw.join("node-store/kv")), 2);
        // Everything local now: the next tick checks and logs nothing.
        assert_eq!(prewarm(), "");
    }

    drop(_guard);
    let _ = std::fs::remove_dir_all(&root);
}
