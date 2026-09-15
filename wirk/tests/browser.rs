//! `wirk browser view|serve` against a real `wirkd`, a real Work and —
//! for `serve` — real loopback TCP connections to the real built
//! binary. No fake daemon and no constructed reply: the Works here are
//! submitted and, where a Claim is needed, actually executed.
//!
//! Each test pins one contract of the browser surface:
//!
//! - a rendered Work says what it is for and where it has got to;
//! - evidence can be followed from the page to the artifact's own bytes,
//!   re-verified against what its Claim was checked against;
//! - an incomplete request is bounded in time and the server carries on
//!   serving afterwards (before the bound existed, one idle client held
//!   the single-threaded accept loop indefinitely);
//! - a scoped read with no named Work does not walk the estate's Work
//!   ids, and a scoped read is refused the Work it is not admitted to;
//! - text recorded in a Work is rendered as content, never as markup or
//!   as a link this page offers to follow;
//! - a forged token and an unknown route learn nothing;
//! - the return action reports what Herdr actually says rather than
//!   asserting an outcome.

#[path = "support/nested_harness.rs"]
mod harness;
#[path = "support/route_fixture.rs"]
mod route_fixture;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harness::{init_repo, materialize_actor, start_wirkd, stop_wirkd, wirk_cli, write_file};
use wirk_core::{ClaimKind, ClaimVerdict, EventKind};
// The `#[path]`-included harness reaches the daemon types through
// `crate::wirkd`, so each including binary brings them into scope.
#[allow(unused_imports)]
use wirk::wirkd;

/// The served bridge's own process, killed when the test ends.
///
/// The bridge is a long-running foreground command that keeps writing to
/// its own stdout and stderr, so whoever pipes those owns them for as
/// long as the process lives. Dropping the read end early does not
/// merely lose output: the next write gets `EPIPE`, and `println!`
/// panics on a failed write, which kills the bridge mid-service. Both
/// pipes are therefore drained on their own thread from the moment the
/// process starts until it exits.
struct ServerChild {
    child: Child,
    /// Everything the bridge wrote to stderr, so a failing assertion can
    /// say what the bridge itself said rather than only that a socket
    /// went away.
    stderr: Arc<Mutex<String>>,
}

impl ServerChild {
    /// The bridge's own account of itself: whether it is still running,
    /// and what it printed. Used in failure messages only.
    fn account(&mut self) -> String {
        let state = match self.child.try_wait() {
            Ok(Some(status)) => format!(
                "the bridge process had already exited (code {:?}, signal {:?})",
                status.code(),
                status.signal()
            ),
            Ok(None) => "the bridge process was still running".to_string(),
            Err(err) => format!("the bridge process could not be waited on: {err}"),
        };
        let said = self.stderr.lock().expect("stderr buffer").clone();
        if said.trim().is_empty() {
            format!("{state}; it wrote nothing to stderr")
        } else {
            format!("{state}; it wrote to stderr:\n{said}")
        }
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads a pipe to exhaustion on its own thread, appending to `sink`.
/// The thread ends when the bridge exits and the pipe reaches EOF.
fn drain<R: Read + Send + 'static>(mut pipe: R, sink: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => sink
                    .lock()
                    .expect("drain sink")
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    });
}

// ---- fixtures ----------------------------------------------------------

/// An Actor Work at its first Waypoint: submitted, never launched — the
/// ordinary "someone has asked for this and it is under way" state.
fn actor_work(estate: &Path, route: &str, intent: &str) -> String {
    route_fixture::write_route(
        estate,
        route,
        &serde_json::json!({
            "id": route,
            "waypoints": [{
                "id": format!("{route}/wp-1"),
                "kind": "Actor",
                "intent": intent,
                "declared_outputs": [{"name": "report.md", "required": true}],
                "boundary": ["**"],
            }],
        })
        .to_string(),
    );
    init_repo(estate);
    harness::submit_kind(estate, route, estate, &["demo:write"], None, Some("actor"))
        .expect("work submit")
        .work_id
}

/// One ad hoc Deterministic Work, actually executed, leaving a validated
/// `Done` Claim over a real `report.md`. Same model-free shape
/// `work_artifact.rs` uses for the same need.
fn completed_work(estate: &Path, text: &str) -> (String, String) {
    let output = wirk_cli()
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--repo",
            "demo:write",
            "--base",
            "deadbeef",
            "--kind",
            "deterministic",
            "--command",
            "sh",
            "-c",
            &format!("printf '%s' {} > report.md", shell_quote(text)),
        ])
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut work = String::new();
    for pair in stdout.split_whitespace().collect::<Vec<_>>().chunks(2) {
        if let [key, value] = pair
            && *key == "work_id"
        {
            work = (*value).to_string();
        }
    }
    assert!(!work.is_empty(), "unexpected submit stdout: {stdout:?}");

    let run = wirk_cli()
        .args(["run-deterministic", "--estate"])
        .arg(estate)
        .args(["--work", &work, "--executor", "child"])
        .output()
        .expect("run-deterministic runs");
    assert!(
        run.status.success(),
        "run-deterministic failed: {}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let claim = harness::journal_events(estate, &work)
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ClaimRecorded {
                claim,
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
                ..
            } => Some(claim.0.clone()),
            _ => None,
        })
        .expect("a validated Done Claim");
    (work, claim)
}

/// A real published source the assembler can actually resolve against:
/// one code file and one knowledge file, committed, acquired and
/// published. Same shape `projection.rs` uses for the same need — a real
/// generation, never a fake.
fn published_source(root: &Path, estate: &Path) -> PathBuf {
    let repo = root.join("source-repo");
    std::fs::create_dir_all(&repo).expect("source repo dir");
    init_repo(&repo);
    std::fs::create_dir_all(repo.join("src")).expect("src dir");
    std::fs::create_dir_all(repo.join("notes")).expect("notes dir");
    write_file(
        &repo,
        "src/server.rs",
        "pub fn claim_boundary_refusal(path: &str) -> bool {\n    // the boundary decision\n    \
         path.starts_with(\"src/\")\n}\n",
    );
    write_file(
        &repo,
        "notes/boundary.md",
        "# Boundary\n\nThe boundary is the declared mutation surface for one Waypoint.\n",
    );
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=browser-test",
            "-c",
            "user.email=browser@example.test",
            "commit",
            "-q",
            "-m",
            "content",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+0000")
                .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+0000")
                .status()
                .expect("git runs")
                .success(),
            "git {args:?} failed"
        );
    }
    let atlas = |args: &[&str]| -> serde_json::Value {
        let mut full = vec!["atlas"];
        full.extend_from_slice(args);
        full.push("--estate");
        full.push(estate.to_str().unwrap());
        full.push("--json");
        let output = wirk_cli().args(&full).output().expect("wirk atlas runs");
        assert!(
            output.status.success(),
            "wirk atlas {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
            .unwrap_or(serde_json::Value::Null)
    };
    let acquired = atlas(&[
        "acquire",
        "--source",
        "demo",
        "--repository",
        repo.to_str().unwrap(),
        "--revision",
        "HEAD",
    ]);
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("acquired generation id")
        .to_string();
    atlas(&["publish", "--source", "demo", "--generation", &generation]);
    repo
}

/// An Actor Work whose Waypoint asks for orientation, materialized so
/// wirkd actually assembles and reserves its World.
fn oriented_work(estate: &Path, repo: &Path, socket: &Path) -> String {
    route_fixture::write_route(
        estate,
        "oriented",
        &serde_json::json!({
            "id": "oriented",
            "waypoints": [{
                "id": "oriented/investigate",
                "kind": "Actor",
                "intent": "Find how the boundary refusal is decided.",
                "declared_outputs": [{"name": "report.md", "required": true}],
                "boundary": ["**"],
                "orient": {
                    "question": "Which function decides whether a Claim is refused for a changed \
                                 path outside the declared boundary? Read src/server.rs and \
                                 notes/boundary.md before answering.",
                    "sources": ["demo"],
                },
            }],
        })
        .to_string(),
    );
    let submitted = harness::submit_kind(
        estate,
        "oriented",
        repo,
        &["demo:write"],
        None,
        Some("actor"),
    )
    .expect("work submit");
    materialize_actor(socket, estate, &submitted.work_id, &submitted.run_id);
    submitted.work_id
}

/// The first `source/…` link the page actually offers, as an address to
/// follow. Tests follow what the page offers rather than constructing an
/// address, which is the only way to check that what it offers works.
fn source_link(body: &str, home: &str) -> Option<String> {
    let needle = format!("\"{home}source/");
    let start = body.find(&needle)? + 1;
    let end = start + body[start..].find('"')?;
    Some(body[start..end].to_string())
}

/// The mixed Work this whole stage-history question comes from: an
/// oriented Actor Waypoint followed by a Deterministic one. Returned
/// with the Actor's own Run id, because that is the stage whose context
/// has to survive the Work moving past it.
fn mixed_work(estate: &Path, repo: &Path, socket: &Path) -> (String, String) {
    route_fixture::write_route(
        estate,
        "mixed",
        &serde_json::json!({
            "id": "mixed",
            "waypoints": [
                {
                    "id": "mixed/investigate",
                    "kind": "Actor",
                    "intent": "Find how the boundary refusal is decided.",
                    "declared_outputs": [{"name": "report.md", "required": true}],
                    "boundary": ["**"],
                    "orient": {
                        "question": "Which function decides whether a Claim is refused for a \
                                     changed path outside the declared boundary? Read \
                                     src/server.rs and notes/boundary.md before answering.",
                        "sources": ["demo"],
                    },
                },
                {
                    "id": "mixed/summarize",
                    "kind": "Deterministic",
                    "command": ["sh", "-c", "printf 'summary' > summary.md"],
                    "declared_outputs": [{"name": "summary.md", "required": true}],
                    "boundary": ["**"],
                },
            ],
        })
        .to_string(),
    );
    let submitted =
        harness::submit_kind(estate, "mixed", repo, &["demo:write"], None, Some("actor"))
            .expect("work submit");
    materialize_actor(socket, estate, &submitted.work_id, &submitted.run_id);
    // An Actor Waypoint's declared output is addressed the managed way,
    // which is what `wirk output dir` prints — the same place a real
    // actor is told to write it.
    let as_actor = |cmd: &mut std::process::Command| {
        cmd.env("WIRK_ESTATE_ROOT", estate)
            .env("WIRK_WORK_ID", &submitted.work_id)
            .env("WIRK_RUN_ID", &submitted.run_id);
    };
    let mut dir_cmd = wirk_cli();
    dir_cmd.args(["output", "dir"]);
    as_actor(&mut dir_cmd);
    let dir_out = dir_cmd.output().expect("wirk output dir runs");
    assert!(
        dir_out.status.success(),
        "wirk output dir failed: {}",
        String::from_utf8_lossy(&dir_out.stderr)
    );
    let outputs = PathBuf::from(String::from_utf8_lossy(&dir_out.stdout).trim().to_string());
    std::fs::create_dir_all(&outputs).expect("managed output dir");
    std::fs::write(
        outputs.join("report.md"),
        "the refusal is decided in claim_boundary_refusal\n",
    )
    .expect("write the actor's declared output");
    let mut claim_cmd = wirk_cli();
    claim_cmd.arg("claim");
    as_actor(&mut claim_cmd);
    let claimed = claim_cmd.output().expect("wirk claim runs");
    assert!(
        claimed.status.success(),
        "the Actor stage could not claim: {}{}",
        String::from_utf8_lossy(&claimed.stdout),
        String::from_utf8_lossy(&claimed.stderr)
    );
    (submitted.work_id, submitted.run_id)
}

/// The source a followed link actually reached — the source page's own
/// heading, which is the path the coordinate names. Two follows of the
/// same link are compared on this.
fn followed_source(body: &str) -> Option<String> {
    let start = body.find("<h1>")? + "<h1>".len();
    let end = start + body[start..].find("</h1>")?;
    Some(body[start..end].to_string())
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn view(estate: &Path, out: &Path, args: &[&str]) -> (bool, String) {
    let output = wirk_cli()
        .args(["browser", "view", "--estate"])
        .arg(estate)
        .args(args)
        .arg("--out")
        .arg(out)
        .output()
        .expect("browser view runs");
    let html = std::fs::read_to_string(out).unwrap_or_default();
    if !output.status.success() {
        return (
            false,
            format!(
                "exit {:?}\nstdout: {}\nstderr: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        );
    }
    (true, html)
}

/// A live `wirk browser serve`, with the URL it announced.
struct Served {
    child: ServerChild,
    addr: String,
    token_path: String,
    token: String,
}

impl Served {
    /// The context a transport failure needs: what the bridge process
    /// was doing and what it said. Without it a dead bridge is
    /// indistinguishable from a transport defect — both are just
    /// `ConnectionReset` at the client.
    fn account(&mut self) -> String {
        self.child.account()
    }
}

fn serve(estate: &Path, work: &str, args: &[&str]) -> Served {
    serve_with_env(estate, work, args, &[])
}

/// The same bridge, with `env` added to its environment — the return
/// control's tests need to disagree with the ambient `HERDR_*` on
/// purpose. One spawn path, so pipe ownership is got right once.
fn serve_with_env(estate: &Path, work: &str, args: &[&str], env: &[(&str, &str)]) -> Served {
    let mut command = wirk_cli();
    command
        .args(["browser", "serve", "--estate"])
        .arg(estate)
        .args(["--work", work])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child: Child = command.spawn().expect("spawn browser serve");
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let said = Arc::new(Mutex::new(String::new()));
    drain(stderr, said.clone());
    let guard = ServerChild {
        child,
        stderr: said,
    };
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read the announce line");
    let url = line
        .trim()
        .rsplit(' ')
        .next()
        .expect("a url on the announce line")
        .to_string();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "unexpected announce line: {line:?}"
    );
    let (addr, rest) = url
        .trim_start_matches("http://")
        .split_once('/')
        .expect("addr and path");
    // The announce line has been read, but the bridge has more to print
    // — the next line, and its idle-timeout notice. Hand the rest of
    // stdout to a drain that lives as long as the process does, rather
    // than letting this `BufReader` close the pipe on the way out.
    drain(reader, Arc::new(Mutex::new(String::new())));
    Served {
        child: guard,
        addr: addr.to_string(),
        token_path: format!("/{rest}"),
        token: rest.trim_end_matches('/').to_string(),
    }
}

/// One request against a live bridge, returned as (head, body).
///
/// A transport failure here reports what the bridge process itself was
/// doing and saying. `ConnectionReset` alone cannot tell a defect in the
/// served transport from a bridge that is no longer running, and reading
/// it as the former cost a whole round of misdiagnosis.
fn request(served: &mut Served, method: &str, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(&served.addr).expect("connect");
    stream
        .write_all(
            format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .expect("write request");
    let mut response = String::new();
    if let Err(err) = stream.read_to_string(&mut response) {
        panic!(
            "{method} {path}: reading the response failed: {err}\n\
             read so far: {response:?}\n{}",
            served.account()
        );
    }
    let mut parts = response.splitn(2, "\r\n\r\n");
    (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    )
}

// ---- 1. what a Work is for, and where it has got to ---------------------

#[test]
fn view_says_what_a_work_is_for_and_where_it_has_got_to() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let work = actor_work(
        &estate,
        "smoke",
        "# Measure the cold start\n\nRun it three times and report the spread.",
    );

    let out: PathBuf = dir.path().join("work.html");
    let (ok, html) = view(&estate, &out, &["--work", &work, "--admin"]);
    assert!(ok, "browser view failed: {html}");

    // The purpose, in the words the Work was admitted with, is the
    // heading — not the id.
    assert!(
        html.contains("Measure the cold start"),
        "the page does not say what this Work is for: {html}"
    );
    // Where it has got to, as a sentence rather than a field dump.
    assert!(
        html.contains("This Work is active"),
        "the page does not say where the Work is: {html}"
    );
    // The World it was given, not just its identity.
    assert!(
        html.contains("The World this Work was given") && html.contains("may write"),
        "the page does not describe the reserved World: {html}"
    );
    // Evidence that is genuinely absent is reported as absent.
    assert!(
        html.contains("No validated Claim has been recorded"),
        "the page does not report the honest empty evidence state: {html}"
    );
    // Nothing on this page needs script to render.
    assert!(
        !html.contains("<script"),
        "the page requires script: {html}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 2. following evidence to the bytes a Claim was checked against -----

#[test]
fn served_evidence_can_be_followed_to_its_actual_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let (work, claim) = completed_work(&estate, "the spread was 1.4s to 2.1s\n");

    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "60"]);
    let home = served.token_path.clone();
    let token = served.token.clone();
    let (head, body) = request(&mut served, "GET", &home);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "unexpected status: {head}"
    );
    // The artifact is a link, not a digest in a table cell.
    let link = format!("/{}/evidence/{claim}/report.md", served.token);
    assert!(
        body.contains(&link),
        "the page does not offer the evidence to follow: {body}"
    );

    let (content_head, content) = request(&mut served, "GET", &link);
    assert!(
        content_head.starts_with("HTTP/1.1 200"),
        "evidence page unreachable: {content_head}"
    );
    assert!(
        content.contains("the spread was 1.4s to 2.1s"),
        "the evidence page does not show the claimed bytes: {content}"
    );
    assert!(
        content.contains("re-read and re-hashed just now"),
        "the evidence page does not say the bytes were re-verified: {content}"
    );

    // An artifact name this Work does not carry is refused without the
    // daemon being asked about it at all.
    let (missing_head, missing) = request(
        &mut served,
        "GET",
        &format!("/{token}/evidence/{claim}/not-claimed.md"),
    );
    assert!(missing_head.starts_with("HTTP/1.1 200"));
    assert!(
        missing.contains("No such artifact on this Work"),
        "an unclaimed name was not refused: {missing}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 3. an incomplete request is bounded, and service continues ---------

/// The transport contract. A client that opens a connection and never
/// finishes its request must not hold the accept loop: the read is
/// bounded in time, the connection is answered and closed, and the next
/// request is served normally.
///
/// Expected red: before the read deadline existed this test never
/// returned from its first `read_to_string` — the single-threaded loop
/// sat in `read_line` until the server's own idle timeout killed it, so
/// the follow-up request below was never answered.
#[test]
fn an_incomplete_request_is_bounded_and_the_server_keeps_serving() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let work = actor_work(&estate, "smoke", "hold a socket open and say nothing");

    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "120"]);
    let home = served.token_path.clone();

    let started = Instant::now();
    let mut idle = TcpStream::connect(&served.addr).expect("connect");
    // A request line that never ends, and then silence.
    idle.write_all(b"GET /never-finished")
        .expect("partial write");
    idle.flush().expect("flush");
    let mut response = String::new();
    idle.read_to_string(&mut response)
        .expect("the server must answer and close");
    let elapsed = started.elapsed();

    assert!(
        response.starts_with("HTTP/1.1 408"),
        "an unfinished request was not bounded; got {response:?} after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "the bound did not hold: {elapsed:?}"
    );

    // Service continues: the very next request is answered normally.
    let (head, body) = request(&mut served, "GET", &home);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "the server stopped serving after an idle client: {head}"
    );
    assert!(body.contains("hold a socket open and say nothing"));
    stop_wirkd(&estate, daemon);
}

/// Root's bounded TCP probe (ROOT-REQUEST-DEADLINE.json, 0362) held an
/// unfinished request open for 6.505s against a server that promised a
/// 5s bound, by sending a few bytes about every 250ms. A per-read
/// timeout is not a whole-request deadline: every arriving byte renewed
/// it, including inside `BufReader::read_line`. The budget has to be
/// spent, not reset. The silent-client case above does not reach this:
/// there, one read blocks for the whole timeout and expires.
#[test]
fn a_request_that_dribbles_bytes_still_hits_the_total_deadline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let work = actor_work(&estate, "smoke", "dribble a request and never finish it");

    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "120"]);
    let home = served.token_path.clone();

    let mut slow = TcpStream::connect(&served.addr).expect("connect");
    let mut reading = slow.try_clone().expect("clone the connection to read it");
    let started = Instant::now();
    slow.write_all(b"GET / HTTP/1.1\r\n")
        .expect("partial write");
    slow.flush().expect("flush");
    // One header byte about every 250ms, never the blank line that would
    // end the request: always something arriving, never a finished
    // request. 80 writes is 20s of dribbling, well past any honest bound.
    let writer = std::thread::spawn(move || {
        for _ in 0..80 {
            if slow.write_all(b"x").is_err() || slow.flush().is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });

    let mut response = Vec::new();
    reading
        .read_to_end(&mut response)
        .expect("the server must answer and close");
    let elapsed = started.elapsed();
    writer.join().ok();
    let response = String::from_utf8_lossy(&response).to_string();

    assert!(
        elapsed < Duration::from_secs(8),
        "a dribbling client held the connection for {elapsed:?}: the total \
         request deadline was not enforced (answered {response:?})"
    );
    assert!(
        response.starts_with("HTTP/1.1 408"),
        "an unfinished request was not answered as timed out after \
         {elapsed:?}; got {response:?}"
    );

    // And the bound did not cost the service: the next request is normal.
    let (head, body) = request(&mut served, "GET", &home);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "the server stopped serving after a slow client: {head}"
    );
    assert!(body.contains("dribble a request and never finish it"));
    stop_wirkd(&estate, daemon);
}

/// `herdr` routes by `HERDR_SOCKET_PATH`; `HERDR_SESSION` is a separate
/// variable an inherited environment often disagrees with. Observed
/// live: a bridge started with an owned socket and an inherited
/// `HERDR_SESSION` focused a pane in the owned session and told the
/// operator it was talking to the inherited one. The page must name the
/// Herdr the button will actually reach.
#[test]
fn the_return_control_names_the_herdr_it_actually_talks_to() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let work = actor_work(&estate, "smoke", "say which Herdr this page reaches");

    let socket = dir.path().join("sessions/owned-session/herdr.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).expect("session dir");

    let mut served = serve_with_env(
        &estate,
        &work,
        &["--admin", "--idle-timeout", "120"],
        &[
            ("HERDR_SOCKET_PATH", socket.to_str().expect("socket path")),
            ("HERDR_SESSION", "an-inherited-session"),
        ],
    );
    let home = served.token_path.clone();
    let (_head, body) = request(&mut served, "GET", &home);
    drop(served);

    assert!(
        body.contains("owned-session"),
        "the page did not name the Herdr its socket reaches: {body}"
    );
    assert!(
        !body.contains("an-inherited-session"),
        "the page named an inherited HERDR_SESSION it does not talk to: {body}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- the delivered context, read as context -----------------------------

/// 0362: the World page was the console report in a `<pre>` — opaque
/// coordinates and a `wirk atlas resolve` command to copy. What a person
/// needs is the selected context itself: where each item came from, why
/// it is there, what it says, and a way to follow it to the actual
/// source. The machine detail stays, behind a disclosure.
#[test]
fn the_delivered_context_reads_as_context_not_as_a_cli_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);
    let repo = published_source(dir.path(), &estate);
    let work = oriented_work(&estate, &repo, &pointer.socket);

    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "120"]);
    let home = served.token_path.clone();
    let (head, body) = request(&mut served, "GET", &format!("{home}world"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    // What it was assembled to answer, in the words it was asked.
    assert!(
        body.contains("path outside the declared boundary"),
        "the question this context answers is not on the page: {body}"
    );
    // Where the delivered material actually is, as a location a person
    // reads — not only as a hex handle.
    assert!(
        body.contains("class=\"where\">src/server.rs:1"),
        "no readable source location on a delivered item: {body}"
    );
    // And what it says, as the item's own excerpt.
    let excerpt = body
        .split("class=\"excerpt\">")
        .nth(1)
        .expect("a delivered item carries an excerpt");
    assert!(
        excerpt.starts_with("pub fn claim_boundary_refusal"),
        "the first item's excerpt is not the source it names: {excerpt:.200}"
    );
    // A way to follow it, rather than a command to copy — and the link
    // names the Run and the revision it was written against, so that
    // following it later cannot mean a different item.
    let offered = source_link(&body, &home).expect("a link from a delivered item to its source");
    assert!(
        offered.starts_with(&format!("{home}source/run-")),
        "the source link does not name the Run it was written against: {offered}"
    );
    // The console report is still available, and is no longer the page.
    assert!(
        body.contains("<details>") && body.contains("wirk world show"),
        "the exact report was dropped rather than moved behind a disclosure: {body}"
    );
    let lede = body
        .split("<details>")
        .next()
        .expect("content before the first disclosure");
    assert!(
        !lede.contains("wirk atlas resolve"),
        "the normal reading still tells a person to copy a CLI command: {lede}"
    );
    stop_wirkd(&estate, daemon);
}

/// Following a delivered item reaches the committed bytes themselves,
/// through the same `atlas resolve` the CLI uses and under this server's
/// own scope.
#[test]
fn a_delivered_item_can_be_followed_to_the_committed_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);
    let repo = published_source(dir.path(), &estate);
    let work = oriented_work(&estate, &repo, &pointer.socket);

    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "120"]);
    let home = served.token_path.clone();
    let (_head, world) = request(&mut served, "GET", &format!("{home}world"));
    let link = source_link(&world, &home).expect("a source link on the World page");

    let (head, body) = request(&mut served, "GET", &link);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        body.contains("claim_boundary_refusal") || body.contains("Boundary"),
        "following an item did not reach its source bytes: {body}"
    );

    // The address carries a position in one named delivered document,
    // never a coordinate: a position that document does not have is
    // refused, and nothing about the estate comes back.
    let stage = link
        .trim_start_matches(&format!("{home}source/"))
        .rsplit_once('/')
        .expect("the link pins a run and a revision")
        .0
        .to_string();
    let (head, body) = request(&mut served, "GET", &format!("{home}source/{stage}/9999"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        body.contains("no item at that position"),
        "an out-of-range item was not refused as one: {body}"
    );

    // A Run that is not this Work's is not addressable at all.
    let (head, body) = request(
        &mut served,
        "GET",
        &format!("{home}source/run-0000000000000000-ff/0/0"),
    );
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        body.contains("not a Run of this Work"),
        "a Run this Work does not have was addressable: {body}"
    );
    stop_wirkd(&estate, daemon);
}

/// A Deterministic stage used to be a blank on the Work page: "no Actor
/// World". What it is, is its command, the commit it runs against and
/// what it owes — all already in the projection.
#[test]
fn a_deterministic_stage_shows_its_command_and_what_it_owes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let (work, _claim) = completed_work(&estate, "deterministic output");

    let out = dir.path().join("deterministic.html");
    let (ok, _) = view(&estate, &out, &["--work", &work, "--admin"]);
    assert!(ok, "view failed");
    let body = std::fs::read_to_string(&out).expect("read the view");

    assert!(
        body.contains("Deterministic stage"),
        "the Deterministic stage is not named: {body}"
    );
    assert!(
        body.contains("run-deterministic") || body.contains("report.md"),
        "neither the command it runs nor what it owes is shown: {body}"
    );
    assert!(
        !body.contains("No Actor World is recorded"),
        "a Deterministic stage is still rendered as a missing Actor World: {body}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 4. a scoped read never walks the estate's Work ids -----------------

/// A caller reading as one Work, naming no target, used to be answered
/// by enumerating every Work id under the estate and rendering a refusal
/// row for each. The ids themselves were the disclosure.
#[test]
fn a_scoped_read_with_no_named_work_does_not_enumerate_the_estate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let mine = actor_work(&estate, "mine", "the Work doing the asking");
    let other = actor_work(&estate, "other", "an unrelated neighbour");

    let out = dir.path().join("scoped.html");
    let (_ok, html) = view(&estate, &out, &["--requesting-work", &mine]);
    assert!(
        !html.contains(&other),
        "a scoped read disclosed an unrelated Work id: {html}"
    );
    assert!(
        html.contains("administrative listing"),
        "the refusal does not say why: {html}"
    );

    // The administrative read is unchanged: it is the estate's listing,
    // and it still lists.
    let admin_out = dir.path().join("admin.html");
    let (ok, admin_html) = view(&estate, &admin_out, &["--admin"]);
    assert!(ok, "the administrative estate map failed");
    assert!(
        admin_html.contains(&other) && admin_html.contains(&mine),
        "the administrative map lost the estate listing: {admin_html}"
    );
    assert!(
        admin_html.contains("an unrelated neighbour"),
        "the estate map is a list of ids rather than of what the Works are for: {admin_html}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 5. admitted versus unadmitted -------------------------------------

#[test]
fn a_scoped_read_is_refused_the_work_it_is_not_admitted_to() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let secret = actor_work(&estate, "secret", "the confidential neighbouring brief");
    let stranger = actor_work(&estate, "stranger", "a Work with no relation to it");

    // Admitted: a Work reading itself.
    let own = dir.path().join("own.html");
    let (ok, own_html) = view(
        &estate,
        &own,
        &["--work", &secret, "--requesting-work", &secret],
    );
    assert!(ok, "a Work could not read itself");
    assert!(
        own_html.contains("the confidential neighbouring brief"),
        "a Work reading itself was denied its own intent: {own_html}"
    );

    // Unadmitted: a stranger asking about it.
    let theirs = dir.path().join("theirs.html");
    let (_ok, theirs_html) = view(
        &estate,
        &theirs,
        &["--work", &secret, "--requesting-work", &stranger],
    );
    assert!(
        !theirs_html.contains("the confidential neighbouring brief"),
        "an unadmitted read returned the Work's content: {theirs_html}"
    );
    assert!(
        theirs_html.contains("InadmissibleEvidence"),
        "the refusal is not reported as a refusal: {theirs_html}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 6. recorded text is content, not markup ---------------------------

/// A Work's own recorded text reaches this page from the estate. It is
/// shown, and it is never markup, never script, and never a link the
/// page offers to follow.
#[test]
fn recorded_text_is_rendered_as_content_never_as_markup_or_a_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let hostile = concat!(
        "<script>alert('x')</script> ",
        "<a href=\"javascript:alert('y')\">follow me</a> ",
        "<img src=x onerror=alert('z')> ",
        "\"><iframe src=//example.test></iframe>"
    );
    let work = actor_work(&estate, "hostile", hostile);

    let out = dir.path().join("hostile.html");
    let (ok, html) = view(&estate, &out, &["--work", &work, "--admin"]);
    assert!(ok, "browser view failed");

    // Shown, as its own characters.
    assert!(
        html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"),
        "the recorded text was not rendered as text: {html}"
    );
    // Never as markup. An `onerror=` inside the escaped run below is
    // characters in a paragraph; what would matter is a tag, and there
    // is none.
    for live in ["<script", "<iframe", "<img"] {
        assert!(
            !html.contains(live),
            "recorded text reached the page as live markup ({live}): {html}"
        );
    }
    assert!(
        html.contains("&lt;img src=x onerror=alert(&#39;z&#39;)&gt;"),
        "the attribute payload was not rendered as its own characters: {html}"
    );
    // Never as a link. Every href this page emits points at one of its
    // own routes; none is built from estate text.
    assert!(
        !html.contains("href=\"javascript:"),
        "recorded text became a link target: {html}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 7. forged and unknown requests ------------------------------------

#[test]
fn a_forged_token_or_unknown_route_learns_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let work = actor_work(
        &estate,
        "smoke",
        "a Work someone might try to reach sideways",
    );
    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "60"]);
    let token = served.token.clone();

    let (head, body) = request(&mut served, "GET", "/not-the-real-token/");
    assert!(head.starts_with("HTTP/1.1 403"), "forged token: {head}");
    assert!(
        !body.contains(&work),
        "a forged request learned the Work id"
    );

    let (head, _) = request(&mut served, "POST", "/not-the-real-token/action/focus");
    assert!(head.starts_with("HTTP/1.1 403"), "forged action: {head}");

    let (head, _) = request(&mut served, "GET", &format!("/{token}/does-not-exist"));
    assert!(head.starts_with("HTTP/1.1 404"), "unknown route: {head}");

    // The action route exists for POST alone; GET is not a way to take
    // it without meaning to.
    let (head, _) = request(&mut served, "GET", &format!("/{token}/action/focus"));
    assert!(head.starts_with("HTTP/1.1 404"), "action via GET: {head}");
    stop_wirkd(&estate, daemon);
}

// ---- 8. the return action reports what Herdr says ----------------------

/// The action is revalidated at click time and reports the outcome it
/// actually got. In this fixture no Herdr agent is named for the Work's
/// Run, so the honest answer is that there is no pane to focus — said
/// plainly, with the Run's own state, rather than a silent no-op.
#[test]
fn the_return_action_reports_what_herdr_actually_says() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let work = actor_work(&estate, "smoke", "a Work with no pane of its own here");
    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "60"]);
    let home = served.token_path.clone();

    let (head, body) = request(&mut served, "POST", &format!("{home}action/focus"));
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "action unreachable: {head}"
    );
    assert!(
        body.contains("is not listing a pane for")
            || body.contains("could not read Herdr&#39;s current agent list"),
        "the action did not report its own outcome: {body}"
    );
    // The copy describes what the button does, including that a pane
    // Herdr kept after its Run ended is still a pane it can focus.
    assert!(
        body.contains("including a pane it kept after the Run"),
        "the return copy does not match what the action does: {body}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 9. the nav offers only what this scope can actually reach --------

/// The estate map is the administrative listing. A bridge serving a
/// scoped read can never produce one, so it does not put a link to it in
/// front of the reader — while the administrative bridge, which can,
/// does. The route itself still answers honestly either way.
#[test]
fn a_scoped_bridge_does_not_offer_a_control_that_would_refuse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, _pointer) = start_wirkd(&estate);
    let work = actor_work(&estate, "smoke", "a Work reading about itself");

    let mut scoped = serve(
        &estate,
        &work,
        &["--requesting-work", &work, "--idle-timeout", "60"],
    );
    let scoped_home = scoped.token_path.clone();
    let scoped_token = scoped.token.clone();
    let (_head, body) = request(&mut scoped, "GET", &scoped_home);
    assert!(
        !body.contains(&format!("/{scoped_token}/estate")),
        "a scoped bridge offered the estate map it cannot produce: {body}"
    );
    let (_head, refused) = request(&mut scoped, "GET", &format!("/{scoped_token}/estate"));
    assert!(
        refused.contains("administrative listing"),
        "the estate route did not refuse a scoped read by name: {refused}"
    );

    let mut admin = serve(&estate, &work, &["--admin", "--idle-timeout", "60"]);
    let admin_home = admin.token_path.clone();
    let (_head, admin_body) = request(&mut admin, "GET", &admin_home);
    assert!(
        admin_body.contains(&format!("/{}/estate", admin.token)),
        "the administrative bridge dropped the estate map it can produce: {admin_body}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 10. a stage's context outlives the stage ---------------------------

/// Expected red: the World page read `current_run_id` and nothing else.
/// The moment this mixed Work advanced past its oriented Actor stage,
/// the context that stage was actually given stopped being reachable
/// from the browser at all — the page answered for the Deterministic Run
/// that is current now, which was never given an orientation. Observed
/// directly on the live page before this test existed.
#[test]
fn a_completed_stages_context_is_still_reachable_after_the_work_advances() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);
    let repo = published_source(dir.path(), &estate);
    let (work, actor_run) = mixed_work(&estate, &repo, &pointer.socket);

    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "120"]);
    let home = served.token_path.clone();

    // The Work has moved on: the current stage is the Deterministic one,
    // and it was never given an orientation.
    let (_head, current) = request(&mut served, "GET", &format!("{home}world"));
    assert!(
        !current.contains("path outside the declared boundary"),
        "the current stage is not the oriented one, but the page shows its question: {current}"
    );

    // The Work page offers every stage, by what the stage is, not only
    // the one that happens to be current.
    let (_head, work_page) = request(&mut served, "GET", &home);
    // And it is headed by what the Work is for. A mixed Work whose
    // current stage is Deterministic has no intent on its *current*
    // World at all, which is how this page came to be headed by the
    // Work's own hash; the intent is on the stage that was given one.
    let heading = work_page
        .split("<h1>")
        .nth(1)
        .and_then(|rest| rest.split("</h1>").next())
        .expect("the page has a heading");
    assert_eq!(
        heading, "Find how the boundary refusal is decided.",
        "the Work page is not headed by what the Work is for"
    );
    assert!(
        !work_page.contains(&format!("<h1>Work {work}")),
        "the Work page is headed by its own identifier: {work_page}"
    );
    assert!(
        work_page.contains("mixed/investigate") && work_page.contains("mixed/summarize"),
        "the Work page does not show both stages: {work_page}"
    );
    assert!(
        work_page.contains(&format!("{home}world/{actor_run}")),
        "the Work page does not offer the completed stage's own context: {work_page}"
    );

    // And that stage's delivered context is still exactly what it was
    // given — the question it was assembled to answer, and its items.
    let (head, stage) = request(&mut served, "GET", &format!("{home}world/{actor_run}"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        stage.contains("path outside the declared boundary"),
        "the completed stage's own question is gone: {stage}"
    );
    assert!(
        stage.contains("class=\"where\">src/server.rs:1"),
        "the completed stage's delivered items are gone: {stage}"
    );
    assert!(
        stage.contains("superseded by a later attempt") || stage.contains("mixed/investigate"),
        "the page does not say which stage this is: {stage}"
    );

    // A Run that is not this Work's is refused, rather than quietly
    // answered for whatever is current.
    let (_head, foreign) = request(
        &mut served,
        "GET",
        &format!("{home}world/run-0000000000000000-ff"),
    );
    assert!(
        foreign.contains("not a Run of this Work"),
        "a Run this Work does not have was answered for: {foreign}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 11. an old source link still means what it meant ------------------

/// Expected red: `source/<n>` resolved `n` against whatever projection
/// the server re-read for the request. Add a revision to the delivered
/// context — which an actor may do at any time with `wirk world expand`
/// — and the same link silently addressed a different document's item
/// `n`. A link has to keep meaning the source it was written against, or
/// say that source is not available; it must not quietly change subject.
#[test]
fn an_older_source_link_still_names_the_item_it_was_written_against() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);
    let repo = published_source(dir.path(), &estate);
    let work = oriented_work(&estate, &repo, &pointer.socket);
    let run = harness::status(&pointer.socket, &work)["run_id"]
        .as_str()
        .expect("a current Run")
        .to_string();

    let mut served = serve(&estate, &work, &["--admin", "--idle-timeout", "120"]);
    let home = served.token_path.clone();
    let (_head, before) = request(&mut served, "GET", &format!("{home}world"));
    let link = source_link(&before, &home).expect("a source link");
    assert!(
        link.starts_with(&format!("{home}source/{run}/0/")),
        "the link does not pin the Run and the revision it was written against: {link}"
    );
    let (_head, was) = request(&mut served, "GET", &link);
    let was_source = followed_source(&was).expect("the followed link names its source");

    // The actor adds a revision to its own delivered context. Nothing
    // already delivered changes; there is simply a later document.
    let expand = wirk_cli()
        .args(["world", "expand", "--reference", "demo:knowledge"])
        .env("WIRK_ESTATE_ROOT", &estate)
        .env("WIRK_WORK_ID", &work)
        .env("WIRK_RUN_ID", &run)
        .output()
        .expect("wirk world expand runs");
    assert!(
        expand.status.success(),
        "world expand failed: {}{}",
        String::from_utf8_lossy(&expand.stdout),
        String::from_utf8_lossy(&expand.stderr)
    );

    // The page now reads the later revision — and the link written
    // against the earlier one still reaches the same source.
    let (_head, after) = request(&mut served, "GET", &format!("{home}world"));
    assert!(
        after.contains(&format!("{home}source/{run}/1/")),
        "the page is not offering links against the revision it is showing: {after}"
    );
    let (head, still) = request(&mut served, "GET", &link);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(
        followed_source(&still).as_deref(),
        Some(was_source.as_str()),
        "an older link changed which source it addresses once a revision was added"
    );

    // And a revision this Run was never given is reported as
    // unavailable, never answered from one it was.
    let (_head, gone) = request(&mut served, "GET", &format!("{home}source/{run}/77/0"));
    assert!(
        gone.contains("no longer available") && gone.contains("has not been replaced"),
        "a revision this Run never had was not reported as unavailable: {gone}"
    );
    stop_wirkd(&estate, daemon);
}

// ---- 12. a related reader sees what it is admitted to, and no more -----

/// Source admission is not the token check.
///
/// A forged token learns nothing because it never reaches an estate read
/// at all; that says nothing about what a reader with a *legitimate*
/// bridge is admitted to. This is the case that does: a nested child
/// Work, genuinely on its container's lineage, reading its container
/// through a real served bridge. The lineage gets it the Work — its
/// purpose, its progress, its Claims. It does not get it the container's
/// checkout-derived content, so the delivered context and every source
/// under it stay withheld, and the page says so rather than showing an
/// empty World as though there were none.
///
/// The requesting Work is the reader's own; nothing here impersonates
/// the container, and the container's own administrative read is
/// unchanged by any of it.
#[test]
fn a_related_reader_is_admitted_to_the_work_and_still_refused_its_sources() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (daemon, pointer) = start_wirkd(&estate);
    let repo = published_source(dir.path(), &estate);

    // The target: a Container Work whose leaf is an oriented Actor, so
    // it has a real delivered context over real committed source. It is
    // bound to `demo` and to `side`.
    route_fixture::write_route(
        &estate,
        "investigation",
        &serde_json::json!({
            "id": "investigation",
            "waypoints": [{
                "id": "outer",
                "kind": "Container",
                "declared_outputs": [{"name": "rollup.md", "required": true}],
                "required_child_outcomes": [{"role": "reviewer", "required": true}],
                "leaves": [{
                    "id": "outer/read",
                    "kind": "Actor",
                    "intent": "Find how the boundary refusal is decided.",
                    "declared_outputs": [{"name": "report.md", "required": true}],
                    "boundary": ["**"],
                    "orient": {
                        "question": "Which function decides whether a Claim is refused for a \
                                     changed path outside the declared boundary? Read \
                                     src/server.rs and notes/boundary.md before answering.",
                        "sources": ["demo"],
                    },
                }],
            }],
        })
        .to_string(),
    );
    let target = harness::submit_kind(
        &estate,
        "investigation",
        &repo,
        &["demo:write", "side:write"],
        None,
        Some("actor"),
    )
    .expect("target work submit");
    materialize_actor(&pointer.socket, &estate, &target.work_id, &target.run_id);

    // The reader: a real child of it, asking for strictly less — `side`
    // and not `demo`. Its own bindings therefore do not cover the
    // target's checkout.
    route_fixture::write_route(
        &estate,
        "review",
        &serde_json::json!({
            "id": "review",
            "waypoints": [{
                "id": "review/wp-1",
                "kind": "Actor",
                "intent": "a nested Work that reads the Work it serves",
                "declared_outputs": [{"name": "review.md", "required": true}],
                "boundary": ["**"],
            }],
        })
        .to_string(),
    );
    let reader = harness::submit_kind(
        &estate,
        "review",
        &repo,
        &["side:write"],
        Some(harness::ParentRef {
            work: &target.work_id,
            waypoint: "outer",
            run: &target.run_id,
            role: "reviewer",
            attempt: None,
        }),
        Some("actor"),
    )
    .expect("reader work submit")
    .work_id;

    // The reader's own bridge onto the Work it is on the lineage of.
    let mut served = serve(
        &estate,
        &target.work_id,
        &["--requesting-work", &reader, "--idle-timeout", "120"],
    );
    let home = served.token_path.clone();

    // Admitted: a real read of the target, not a refusal.
    let (head, work_page) = request(&mut served, "GET", &home);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        !work_page.contains("InadmissibleEvidence"),
        "a Work on the lineage was refused outright rather than narrowed: {work_page}"
    );
    assert!(
        work_page.contains("outer/read"),
        "the related reader was not given the target's own progress: {work_page}"
    );

    // Withheld: the target's delivered context, and with it every source
    // under it. Said plainly, and not as "there is no World".
    let (head, world) = request(&mut served, "GET", &format!("{home}world"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        world.contains("not disclosed to this reader"),
        "the withheld World was not reported as withheld: {world}"
    );
    assert!(
        !world.contains("path outside the declared boundary"),
        "a withheld World's authored question reached a related reader: {world}"
    );
    assert!(
        !world.contains("claim_boundary_refusal"),
        "a withheld World's delivered source text reached a related reader: {world}"
    );

    // And the source route under it is refused for the same reason,
    // rather than resolved because the address was well formed.
    let (head, source) = request(
        &mut served,
        "GET",
        &format!("{home}source/{}/0/0", target.run_id),
    );
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        source.contains("not disclosed to this reader"),
        "a withheld World's source was not refused: {source}"
    );
    assert!(
        !source.contains("claim_boundary_refusal"),
        "committed source bytes reached a reader the World was withheld from: {source}"
    );

    // The target's own administrative read is untouched by any of this:
    // withholding narrowed one reader, not the estate.
    let mut admin = serve(
        &estate,
        &target.work_id,
        &["--admin", "--idle-timeout", "120"],
    );
    let admin_home = admin.token_path.clone();
    let (_head, admin_world) = request(&mut admin, "GET", &format!("{admin_home}world"));
    assert!(
        admin_world.contains("path outside the declared boundary"),
        "narrowing one reader also narrowed the administrative read: {admin_world}"
    );
    stop_wirkd(&estate, daemon);
}
