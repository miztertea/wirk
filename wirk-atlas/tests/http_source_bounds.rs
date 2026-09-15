//! What an HTTP acquisition does with a resource an operator explicitly
//! admitted, observed against a real HTTP server on this host rather
//! than against a fake.
//!
//! Ruling 0401 removed the built-in 8 MiB response bound and the 20
//! second transfer clock: neither was a transport refusal this host
//! makes, both were numbers this product chose, and a resource an
//! operator named is fetched as the resource it is. What an operator
//! *does* configure is still enforced, by `curl` itself, and still
//! refused by name.
//!
//! The server is `python3 -m http.server` bound to loopback on a port
//! the kernel assigns, serving one directory this check owns and
//! removes. No network beyond loopback, and no fixture outside the
//! temporary directory.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use tempfile::TempDir;
use wirk_atlas::{AcquireOutcome, AtlasStore, ExtractorPolicy, SourceGeneration};

/// A real local HTTP server over `root`, killed when this is dropped.
struct LocalServer {
    child: Child,
    port: u16,
}

impl LocalServer {
    fn serving(root: &Path) -> Option<Self> {
        // Port 0 lets the kernel pick; `--bind 127.0.0.1` keeps it off
        // every other interface. The chosen port is read back from the
        // banner python writes on startup rather than guessed.
        let mut child = Command::new("python3")
            // `-u` is load-bearing: without it the banner sits in
            // python's own stdout buffer and the read below never
            // returns, because the pipe is not a terminal.
            .args(["-u", "-m", "http.server", "0", "--bind", "127.0.0.1"])
            .current_dir(root)
            .stdout(Stdio::piped())
            // Request logs go to stderr and nothing here reads them; a
            // pipe nobody drains fills and stalls the server, so they go
            // to the null device instead.
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // The banner is on *stdout* — `http.server` prints it with
        // `print()`, and only its request log goes to stderr.
        let stdout = child.stdout.take()?;
        let mut lines = BufReader::new(stdout).lines();
        let banner = lines.next()?.ok()?;
        // "Serving HTTP on 127.0.0.1 port 39481 (http://127.0.0.1:39481/) ..."
        let port: u16 = banner
            .split_whitespace()
            .skip_while(|word| *word != "port")
            .nth(1)?
            .parse()
            .ok()?;
        // Readiness is a connection that succeeds, not a sleep.
        for _ in 0..200 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Some(LocalServer { child, port });
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let _ = child.kill();
        None
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The acquisition reports identity and coverage; the generation's own
/// resource list lives in the immutable generation directory, which is
/// what these checks read it back from.
fn read_staged(atlas: &AtlasStore, outcome: AcquireOutcome) -> SourceGeneration {
    match outcome {
        AcquireOutcome::Staged(staged) => atlas
            .generation(&staged.id)
            .expect("the generation just staged reads back"),
        other => panic!("expected Staged, got {other:?}"),
    }
}

/// A response well past the byte count that used to be the built-in
/// `http_max_response_bytes` is acquired, in full, and recorded at the
/// digest of exactly the bytes that arrived.
///
/// Watched failing against the previous defaults, where this acquisition
/// returned "response from http://127.0.0.1:…/large.md exceeds the
/// 8388608-byte bounded response size" — `curl` exit 63, the transfer
/// aborted by an option this product filled in on the operator's behalf.
///
/// 9 MiB is the stimulus that makes the old refusal reachable, not a
/// threshold this asserts anything about.
#[test]
fn a_response_past_the_old_built_in_bound_is_acquired_whole() {
    let served = TempDir::new().unwrap();
    let mut body = String::from("# large\n\n");
    while body.len() < 9 * 1024 * 1024 {
        body.push_str("a line of ordinary prose served over HTTP\n");
    }
    std::fs::write(served.path().join("large.md"), &body).unwrap();
    let Some(server) = LocalServer::serving(served.path()) else {
        eprintln!("skipped: no python3 http.server on this host, so the real service is absent");
        return;
    };

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-http").unwrap();
    let membership = atlas
        .register_http("site", &server.url("large.md"), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_http(&membership, "current", ExtractorPolicy::default())
            .expect("a large response is acquired, not refused for its size");
        read_staged(&atlas, outcome)
    };
    let resource = generation
        .resources
        .first()
        .expect("one fetch produces one resource");
    assert_eq!(
        resource.byte_len,
        Some(body.len() as u64),
        "the whole response was read, not a bounded prefix"
    );
    let expected: String = <sha2::Sha256 as sha2::Digest>::digest(body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        generation.revision, expected,
        "the identity is the digest of exactly the bytes that arrived"
    );
}

/// The same server and the same resource against an estate that *asked*
/// for a bound: `curl` aborts the transfer and the refusal names the
/// setting, exactly as it always did. Removing a default is not removing
/// the mechanism.
#[test]
fn a_configured_response_bound_still_refuses_by_name() {
    let served = TempDir::new().unwrap();
    std::fs::write(served.path().join("small.md"), "# small\n\n".repeat(64)).unwrap();
    let Some(server) = LocalServer::serving(served.path()) else {
        eprintln!("skipped: no python3 http.server on this host, so the real service is absent");
        return;
    };

    let estate = TempDir::new().unwrap();
    std::fs::create_dir_all(estate.path().join(".wirk")).unwrap();
    std::fs::write(
        estate.path().join(".wirk").join("resources.json"),
        r#"{ "http_max_response_bytes": 16 }"#,
    )
    .unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-http-bounded").unwrap();
    let membership = atlas
        .register_http("site", &server.url("small.md"), "current")
        .unwrap();
    let error = atlas
        .acquire_http(&membership, "current", ExtractorPolicy::default())
        .expect_err("a configured response bound still refuses");
    let detail = error.to_string();
    assert!(
        detail.contains("bounded response size this estate configured"),
        "the refusal names whose bound it was: {detail}"
    );
}
