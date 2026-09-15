//! What an HTTP acquisition does with a redirect chain, observed against
//! a real local HTTP server rather than a fake.
//!
//! Ruling 0403: a finite chain the origin actually serves must remain
//! usable, and a chain that returns to a URL it has already served has
//! to end with a truthful, deterministic answer. The earlier candidate
//! passed `curl -L --max-redirs -1` and delegated every cycle to
//! eventual cancellation.
//!
//! What is checked here is exactly the condition the implementation
//! claims and no more: a **repeated destination**. A server emitting an
//! unbounded sequence of distinct URLs is still non-terminating, and is
//! still ended by cancellation or by an explicit transfer budget — both
//! of which are also exercised below.
//!
//! The server is a `python3` `ThreadingHTTPServer` bound to loopback on
//! a kernel-assigned port, serving only the routes this file needs. No
//! network beyond loopback, and every fixture is removed with the
//! temporary directory that owns it.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use tempfile::TempDir;
use wirk_atlas::{AcquireOutcome, AtlasStore, ExtractorPolicy};

const SERVER: &str = r##"
import http.server, os, sys, time

MARKER = os.environ.get("WIRK_HANG_MARKER", "")
BODY = b"# final\n\nthe redirectmarker line.\n"

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def redirect(self, target):
        self.send_response(302)
        self.send_header("Location", target)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self):
        path = self.path
        if path.startswith("/chain/"):
            remaining = int(path.rsplit("/", 1)[1])
            self.redirect("/final.md" if remaining <= 1 else "/chain/%d" % (remaining - 1))
            return
        if path == "/loop/a":
            self.redirect("/loop/b")
            return
        if path == "/loop/b":
            self.redirect("/loop/a")
            return
        if path.startswith("/slow/"):
            # A hop that genuinely takes time to answer, because the
            # server holds it: the delay is the mechanism under test, not
            # the acceptance criterion. Each visit is recorded so the
            # check can assert which hops were actually reached.
            remaining = int(path.rsplit("/", 1)[1])
            if MARKER:
                with open(MARKER + ".slow-%d" % remaining, "w") as handle:
                    handle.write("reached")
            time.sleep(0.6)
            self.redirect("/final.md" if remaining <= 1 else "/slow/%d" % (remaining - 1))
            return
        if path == "/hang":
            if MARKER:
                with open(MARKER, "w") as handle:
                    handle.write("reached")
            while True:
                time.sleep(0.05)
        if path == "/final.md":
            self.send_response(200)
            self.send_header("Content-Type", "text/markdown")
            self.send_header("Content-Length", str(len(BODY)))
            self.end_headers()
            self.wfile.write(BODY)
            return
        self.send_response(404)
        self.send_header("Content-Length", "0")
        self.end_headers()

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print("port %d" % server.server_address[1], flush=True)
server.serve_forever()
"##;

/// A real local redirect server, killed when this is dropped.
struct RedirectServer {
    child: Child,
    port: u16,
    _scratch: TempDir,
    marker: std::path::PathBuf,
}

impl RedirectServer {
    fn start() -> Option<Self> {
        let scratch = TempDir::new().ok()?;
        let marker = scratch.path().join("hang-reached");
        // `-u` is load-bearing: without it the banner sits in python's
        // own stdout buffer and the read below never returns, because
        // the pipe is not a terminal.
        let mut child = Command::new("python3")
            .args(["-u", "-c", SERVER])
            .env("WIRK_HANG_MARKER", &marker)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let banner = BufReader::new(stdout).lines().next()?.ok()?;
        let port: u16 = banner.split_whitespace().nth(1)?.parse().ok()?;
        // Readiness is a connection that succeeds, not a sleep.
        for _ in 0..400 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Some(RedirectServer {
                    child,
                    port,
                    _scratch: scratch,
                    marker,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
        None
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Whether the server actually served `/slow/<n>` — real state it
    /// recorded, so a check can say which hops were reached rather than
    /// inferring it from how long something took.
    fn reached_slow(&self, remaining: u32) -> bool {
        let mut marker = self.marker.clone().into_os_string();
        marker.push(format!(".slow-{remaining}"));
        std::path::PathBuf::from(marker).exists()
    }
}

impl Drop for RedirectServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // Checked, not assumed: the owned fixture really exited.
        let status = self.child.wait();
        assert!(
            status.is_ok(),
            "the owned redirect server must be reaped: {status:?}"
        );
    }
}

fn estate_with(policy: Option<&str>) -> TempDir {
    let estate = TempDir::new().unwrap();
    if let Some(body) = policy {
        std::fs::create_dir_all(estate.path().join(".wirk")).unwrap();
        std::fs::write(estate.path().join(".wirk").join("resources.json"), body).unwrap();
    }
    estate
}

fn staging_entries(estate: &Path) -> Vec<String> {
    let atlas = estate.join("atlas");
    std::fs::read_dir(&atlas)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.starts_with(".tmp-"))
                .collect()
        })
        .unwrap_or_default()
}

/// A finite chain the origin actually serves is followed to its end, and
/// the generation records the final response's own identity.
#[test]
fn a_finite_redirect_chain_is_followed_to_its_end() {
    let Some(server) = RedirectServer::start() else {
        eprintln!("skipped: no python3 on this host, so the real service is absent");
        return;
    };
    let estate = estate_with(None);
    let mut atlas = AtlasStore::open(estate.path(), "estate-redirects").unwrap();
    let membership = atlas
        .register_http("site", &server.url("/chain/3"), "current")
        .unwrap();
    let AcquireOutcome::Staged(staged) = atlas
        .acquire_http(&membership, "current", ExtractorPolicy::default())
        .expect("a finite chain is an ordinary fetch")
    else {
        panic!("expected the chain to reach a real response");
    };
    let origin = staged.origin.as_deref().expect("an HTTP origin");
    assert_eq!(origin.requested_url, server.url("/chain/3"));
    assert_eq!(
        origin.final_url,
        server.url("/final.md"),
        "the recorded final URL is where the chain actually ended"
    );
    assert_eq!(origin.status, 200);
    assert_eq!(
        staged.coverage.indexed, 1,
        "the final response's own content is what got indexed: {:?}",
        staged.coverage
    );
    // The identity is the digest of exactly the bytes the last hop
    // served, never a redirect body.
    let expected = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"# final\n\nthe redirectmarker line.\n");
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    assert_eq!(staged.revision, expected);
    assert!(
        staging_entries(estate.path()).is_empty(),
        "the fetch's temporary body is removed either way"
    );
}

/// A chain that points back at a URL this fetch has already requested is
/// refused by name, and it refuses **by itself** — no cancellation, no
/// clock, no chosen hop count.
///
/// What `16840cf` actually did, measured against this same two-URL cycle
/// on the installed `curl 8.18.0` rather than assumed: `-L --max-redirs
/// -1` did not run forever. It made roughly five thousand real requests
/// to the origin and then exited 100 with `curl: (100) Too many response
/// headers, 5000 is max` — an internal guard of the transport, reported
/// by the old code as `SourceBytesUnavailable("curl for <url> exited
/// 100: ...")`. So the defect is not non-termination on this host; it is
/// thousands of requests followed by an answer that names neither the
/// cycle nor anything the operator can act on. Both halves are corrected
/// here: two requests, and a refusal that says what it found.
#[test]
fn a_repeated_redirect_destination_is_refused_as_a_cycle() {
    let Some(server) = RedirectServer::start() else {
        eprintln!("skipped: no python3 on this host, so the real service is absent");
        return;
    };
    let estate = estate_with(None);
    let mut atlas = AtlasStore::open(estate.path(), "estate-cycle").unwrap();
    let membership = atlas
        .register_http("site", &server.url("/loop/a"), "current")
        .unwrap();
    let error = atlas
        .acquire_http(&membership, "current", ExtractorPolicy::default())
        .expect_err("a redirect cycle is refused, not followed");
    let detail = error.to_string();
    assert!(
        detail.contains("redirect cycle"),
        "the refusal names the condition it actually detected: {detail}"
    );
    assert!(
        detail.contains("already requested"),
        "and says why the chain cannot terminate: {detail}"
    );
    assert!(
        staging_entries(estate.path()).is_empty(),
        "a refused fetch leaves no staged body behind"
    );
}

/// An explicit `http_max_redirects` still means what it says, applied to
/// the hop count across the chain rather than handed to `curl -L`.
#[test]
fn a_configured_redirect_bound_refuses_a_longer_chain_by_name() {
    let Some(server) = RedirectServer::start() else {
        eprintln!("skipped: no python3 on this host, so the real service is absent");
        return;
    };
    let estate = estate_with(Some(r#"{ "http_max_redirects": 1 }"#));
    let mut atlas = AtlasStore::open(estate.path(), "estate-hops").unwrap();
    let membership = atlas
        .register_http("site", &server.url("/chain/3"), "current")
        .unwrap();
    let error = atlas
        .acquire_http(&membership, "current", ExtractorPolicy::default())
        .expect_err("a chain longer than the configured bound is refused");
    assert!(
        error.to_string().contains("more than the 1 redirect"),
        "the refusal names whose bound it was: {error}"
    );

    // And the same estate still follows a chain that fits inside it.
    let short = atlas
        .register_http("short", &server.url("/chain/1"), "current")
        .unwrap();
    let AcquireOutcome::Staged(_) = atlas
        .acquire_http(&short, "current", ExtractorPolicy::default())
        .expect("one hop is within a one-hop bound")
    else {
        panic!("expected a staged generation");
    };
}

/// `http_timeout_secs: 0` is a transfer budget that has already run out.
/// It is applied as written — the fetch does not start — rather than
/// handed to `curl --max-time 0`, which libcurl documents as "no
/// timeout": the opposite of what was asked for.
#[test]
fn an_explicit_zero_transfer_budget_refuses_before_the_transfer() {
    let Some(server) = RedirectServer::start() else {
        eprintln!("skipped: no python3 on this host, so the real service is absent");
        return;
    };
    let estate = estate_with(Some(r#"{ "http_timeout_secs": 0 }"#));
    let mut atlas = AtlasStore::open(estate.path(), "estate-expired").unwrap();
    let membership = atlas
        .register_http("site", &server.url("/final.md"), "current")
        .unwrap();
    let error = atlas
        .acquire_http(&membership, "current", ExtractorPolicy::default())
        .expect_err("an expired transfer budget refuses");
    let detail = error.to_string();
    assert!(
        detail.contains("http_timeout_secs 0"),
        "the refusal names the value the operator wrote: {detail}"
    );
    assert!(
        detail.contains("was not fetched"),
        "and says the transfer did not happen: {detail}"
    );
}

/// An explicit `http_timeout_secs` bounds the **fetch**, not each hop.
///
/// The claim is about accounting, so it is checked causally rather than
/// by timing: the same two-hop chain, where each hop takes about 0.6s,
/// is fetched twice against the same server. A budget of 1 second is
/// larger than either hop and smaller than their sum — a per-hop clock
/// would let both through, a whole-chain budget cannot — and a budget of
/// 10 seconds comfortably covers both. The assertion is on which one
/// succeeds and which one refuses, never on how long anything took.
///
/// The arithmetic behind it is pinned without any clock at all in
/// `http_source::tests::each_hop_receives_the_remainder_of_one_budget_not_a_fresh_copy`;
/// this is the half of the same statement that only a real transfer can
/// show.
#[test]
fn one_explicit_budget_spans_the_whole_chain_rather_than_restarting_each_hop() {
    let Some(server) = RedirectServer::start() else {
        eprintln!("skipped: no python3 on this host, so the real service is absent");
        return;
    };
    // Positive control first: with a budget that covers the whole chain,
    // this exact route is an ordinary successful fetch. Without it, the
    // refusal below would prove only that something was broken.
    let generous = estate_with(Some(r#"{ "http_timeout_secs": 10 }"#));
    let mut atlas = AtlasStore::open(generous.path(), "estate-budget-generous").unwrap();
    let membership = atlas
        .register_http("site", &server.url("/slow/2"), "current")
        .unwrap();
    let AcquireOutcome::Staged(staged) = atlas
        .acquire_http(&membership, "current", ExtractorPolicy::default())
        .expect("a budget covering the chain lets the chain complete")
    else {
        panic!("expected the chain to reach a real response");
    };
    assert_eq!(
        staged.origin.as_deref().expect("an origin").final_url,
        server.url("/final.md"),
        "both hops were followed"
    );
    // Both hops really were served, so the two arms differ in the budget
    // and in nothing else.
    assert!(server.reached_slow(2) && server.reached_slow(1));

    let tight = estate_with(Some(r#"{ "http_timeout_secs": 1 }"#));
    let mut atlas = AtlasStore::open(tight.path(), "estate-budget-tight").unwrap();
    let membership = atlas
        .register_http("site", &server.url("/slow/2"), "current")
        .unwrap();
    // The refusal arrives either as this policy's own exhausted-budget
    // error or, when the remainder was still expressible, as `curl`
    // ending the hop it was given that remainder for. Both are the same
    // fact — one budget, spent across the chain — so both are accepted,
    // and what is asserted is that the fetch did not succeed.
    let detail = match atlas.acquire_http(&membership, "current", ExtractorPolicy::default()) {
        Ok(AcquireOutcome::Unavailable(detail)) => detail,
        Err(error) => error.to_string(),
        Ok(AcquireOutcome::Staged(staged)) => panic!(
            "one budget cannot cover two hops that each nearly fill it, yet this staged {:?}",
            staged.id
        ),
    };
    assert!(
        detail.contains("transfer budget this estate configured") || detail.contains("exited 28"),
        "the refusal is the operator's budget running out across the chain, by this policy's \
         own accounting or by curl's on the remainder it was given: {detail}"
    );
    assert!(
        staging_entries(tight.path()).is_empty(),
        "a refused fetch leaves no staged body behind"
    );
}

/// Ordinary cancellation still reaches a fetch that will never finish on
/// its own — the case a cycle refusal deliberately does *not* claim to
/// have replaced.
///
/// The readiness signal is the server's own marker file, written when it
/// has actually received the request, not an elapsed window.
#[test]
fn a_fetch_that_never_completes_is_still_reached_by_cancellation() {
    let Some(server) = RedirectServer::start() else {
        eprintln!("skipped: no python3 on this host, so the real service is absent");
        return;
    };
    let estate = estate_with(None);
    let mut atlas = AtlasStore::open(estate.path(), "estate-cancel").unwrap();
    let membership = atlas
        .register_http("site", &server.url("/hang"), "current")
        .unwrap();
    // Taken before the store moves into the fetching thread, exactly as
    // the daemon's cancel verb holds it.
    let registry = atlas.job_registry();
    let root = estate.path().to_path_buf();
    let marker = server.marker.clone();

    let fetching = std::thread::spawn({
        let membership = membership.clone();
        move || {
            let outcome = atlas.acquire_http(&membership, "current", ExtractorPolicy::default());
            (atlas, outcome)
        }
    });

    // Real state, not a sleep: the server says it has the request.
    let mut reached = false;
    for _ in 0..1200 {
        if marker.exists() && !registry.list().is_empty() {
            reached = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        reached,
        "the fetch must actually be in flight to be cancelled"
    );
    let running = registry.list();
    assert_eq!(
        running[0].scope, membership.alias,
        "the job names its source alias, which is the cancellation target"
    );

    let acknowledged = registry.cancel(
        &wirk_core::jobs::JobSelector::Scope(membership.alias.clone()),
        "was cancelled by an operator (test)",
    );
    assert_eq!(acknowledged.len(), 1, "the running fetch was reached");

    let (_atlas, outcome) = fetching.join().expect("the fetching thread ends");
    let error = outcome.expect_err("a cancelled fetch is not a staged generation");
    assert!(
        error.to_string().contains("cancelled by an operator"),
        "the refusal names the deliberate act: {error}"
    );
    assert!(
        staging_entries(&root).is_empty(),
        "a cancelled fetch leaves no staged body behind"
    );
}
