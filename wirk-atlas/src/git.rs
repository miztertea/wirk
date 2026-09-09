use crate::{AtlasError, CoverageDisposition, ExtractorPolicy, GenerationId, ResourceRecord};
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[String]) -> Result<Vec<u8>, AtlasError> {
    let output = Command::new("git")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(AtlasError::GitUnavailable(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}
fn text(repo: &Path, args: &[String]) -> Result<String, AtlasError> {
    String::from_utf8(git(repo, args)?)
        .map(|s| s.trim().to_owned())
        .map_err(|_| AtlasError::GitUnavailable("non UTF-8 Git object identifier".into()))
}
pub(crate) fn commit_and_tree(
    repo: &Path,
    requested: &str,
) -> Result<(String, String), AtlasError> {
    let commit = text(
        repo,
        &[
            "rev-parse".into(),
            "--verify".into(),
            format!("{requested}^{{commit}}"),
        ],
    )?;
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AtlasError::GitUnavailable(
            "Git did not return a full SHA-1 commit".into(),
        ));
    }
    let tree = text(
        repo,
        &[
            "rev-parse".into(),
            "--verify".into(),
            format!("{commit}^{{tree}}"),
        ],
    )?;
    Ok((commit, format!("sha1:{tree}")))
}
pub(crate) fn resources(
    repo: &Path,
    commit: &str,
    generation: &GenerationId,
    policy: &ExtractorPolicy,
) -> Result<Vec<ResourceRecord>, AtlasError> {
    let raw = git(
        repo,
        &[
            "ls-tree".into(),
            "-r".into(),
            "-z".into(),
            "-l".into(),
            commit.into(),
        ],
    )?;
    let mut records = Vec::new();
    for entry in raw.split(|b| *b == 0).filter(|x| !x.is_empty()) {
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            return Err(AtlasError::GitUnavailable(
                "invalid NUL-framed ls-tree record".into(),
            ));
        };
        let meta = std::str::from_utf8(&entry[..tab])
            .map_err(|_| AtlasError::GitUnavailable("invalid Git metadata".into()))?;
        let fields: Vec<_> = meta.split_whitespace().collect();
        if fields.len() != 4 {
            return Err(AtlasError::GitUnavailable("invalid Git tree fields".into()));
        }
        let path = entry[tab + 1..].to_vec();
        let mode = fields[0].to_owned();
        let oid = fields[2].to_owned();
        let size = fields[3].parse::<u64>().ok();
        let (disposition, detail, units) = if mode == "160000" {
            (
                CoverageDisposition::Unsupported,
                Some("gitlink".into()),
                vec![],
            )
        } else if mode == "120000" {
            (
                CoverageDisposition::Unsupported,
                Some("symlink".into()),
                vec![],
            )
        } else if ExtractorPolicy::excluded(&path) {
            (
                CoverageDisposition::Excluded,
                Some("fixed secret-like policy".into()),
                vec![],
            )
        } else if !policy.supports(&path) {
            (
                CoverageDisposition::Unsupported,
                Some("no extractor for path family".into()),
                vec![],
            )
        } else {
            match git(repo, &["cat-file".into(), "blob".into(), oid.clone()]) {
                Ok(bytes) if bytes.contains(&0) => (
                    CoverageDisposition::Unsupported,
                    Some("binary blob".into()),
                    vec![],
                ),
                Ok(bytes) => match policy.units(generation, &path, &oid, &bytes) {
                    Ok(units) => (CoverageDisposition::Indexed, None, units),
                    Err(detail) => (CoverageDisposition::Error, Some(detail.into()), vec![]),
                },
                Err(AtlasError::GitUnavailable(detail)) => {
                    (CoverageDisposition::Unavailable, Some(detail), vec![])
                }
                Err(error) => return Err(error),
            }
        };
        records.push(ResourceRecord {
            path,
            mode,
            object_id: Some(oid),
            byte_len: size,
            disposition,
            detail,
            units,
        });
    }
    records.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(records)
}
pub(crate) fn blob(repo: &Path, oid: &str) -> Result<Vec<u8>, AtlasError> {
    git(repo, &["cat-file".into(), "blob".into(), oid.into()])
}

/// Same committed bytes as `blob`, one object at a time, but over a
/// single long-lived `git cat-file --batch-command` session instead of
/// one process spawn per object. Reads exactly the objects named by
/// `oids`, in order, and returns exactly what `blob` would have returned
/// for each: on a missing or otherwise unreadable object this fails with
/// the same `AtlasError::GitUnavailable` shape a lone `blob(repo, oid)`
/// call for that object would have produced, so a caller that already
/// tolerates that error does not need to change how it reacts.
pub(crate) fn blobs(
    repo: &Path,
    oids: &[String],
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, AtlasError> {
    blobs_with_program(Path::new("git"), repo, oids)
}

/// How much of a failing child's own account is kept. A `git` that talks
/// past this is still drained to the end -- the point of the bound is that
/// this process's memory is not the child's to decide, not that the child
/// is allowed to wedge us by talking.
const BATCH_STDERR_KEPT: usize = 8 * 1024;

/// `blobs`, with the program named explicitly so a test can hand it a
/// stand-in child whose behaviour (a talkative stderr, a truncated
/// answer) a real `git` only produces under conditions a test cannot
/// arrange.
pub(crate) fn blobs_with_program(
    program: &Path,
    repo: &Path,
    oids: &[String],
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, AtlasError> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;
    let mut results = std::collections::BTreeMap::new();
    if oids.is_empty() {
        return Ok(results);
    }
    let mut child = Command::new(program)
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch-command", "--buffer"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let requested = oids.to_vec();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for oid in &requested {
            writeln!(stdin, "contents {oid}")?;
        }
        stdin.flush()
        // `stdin` is dropped here, which is the batch session's own end of
        // input: `--buffer` flushes and the child exits on it.
    });
    // Drained on its own thread, and to the end, because this side reads
    // the child's stdout to completion before it waits for the child: a
    // stderr pipe nobody is emptying fills at the operating system's
    // buffer (64KiB here) and blocks the child mid-write, which blocks
    // its stdout, which blocks the read below -- a hang, not an error,
    // inside the daemon thread serving the query.
    let mut stderr_handle = child.stderr.take().expect("stderr was piped");
    let drain = std::thread::spawn(move || -> Vec<u8> {
        let mut kept = Vec::new();
        let mut chunk = [0u8; 8 * 1024];
        loop {
            match stderr_handle.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if kept.len() < BATCH_STDERR_KEPT {
                        let room = BATCH_STDERR_KEPT - kept.len();
                        kept.extend_from_slice(&chunk[..read.min(room)]);
                    }
                }
            }
        }
        kept
    });
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut reader = BufReader::new(stdout);
    let mut read_error: Option<AtlasError> = None;
    for oid in oids {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) => {
                read_error = Some(AtlasError::GitUnavailable(format!(
                    "git cat-file --batch-command closed its output before object {oid}"
                )));
                break;
            }
            Ok(_) => {}
            Err(error) => {
                read_error = Some(error.into());
                break;
            }
        }
        let header = header.trim_end_matches('\n');
        if header == format!("{oid} missing") {
            // Same as `blob(repo, oid)` on an object Git cannot find: no
            // entry for this oid, not a hard stop -- a caller that reads
            // every requested object one at a time already has to handle
            // "this one wasn't there" per object, so the batched read
            // keeps going and reports it the same way, by omission.
            continue;
        }
        let mut fields = header.split_whitespace();
        let (Some(got), Some(_kind), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
            read_error = Some(AtlasError::GitUnavailable(format!(
                "malformed git cat-file --batch-command header for {oid}: {header:?}"
            )));
            break;
        };
        let Ok(size) = size.parse::<usize>() else {
            read_error = Some(AtlasError::GitUnavailable(format!(
                "non-numeric object size from git cat-file --batch-command for {oid}"
            )));
            break;
        };
        if got != oid {
            read_error = Some(AtlasError::GitUnavailable(format!(
                "git cat-file --batch-command answered object {got} for requested {oid}"
            )));
            break;
        }
        let mut bytes = vec![0u8; size];
        if let Err(error) = reader.read_exact(&mut bytes) {
            read_error = Some(error.into());
            break;
        }
        let mut trailing = [0u8; 1];
        if let Err(error) = reader.read_exact(&mut trailing) {
            read_error = Some(error.into());
            break;
        }
        results.insert(oid.clone(), bytes);
    }
    // Closing this side of the pipe is what unblocks a child still writing
    // an answer nobody is going to read, so it is dropped before the two
    // threads are joined and before the child is waited for.
    drop(reader);
    let write_error = match writer.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some(std::io::Error::other(
            "the thread feeding git cat-file --batch-command panicked",
        )),
    };
    let stderr = drain.join().unwrap_or_default();
    let status = child.wait()?;
    let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
    if let Some(error) = read_error {
        // A short/malformed read is ambiguous on its own: it is exactly
        // what a `-C <repo>` that failed to open (repo missing, not a
        // Git repository, permissions) also looks like from here -- git
        // exits nonzero before writing any object header at all. Prefer
        // the process's own account when it has one, the same
        // `AtlasError::GitUnavailable(stderr)` a lone `blob(repo, oid)`
        // call against that repo would already have produced, so a
        // caller that tolerates "this source is unavailable" continues
        // to see that, not a new, narrower failure shape.
        if !status.success() {
            return Err(AtlasError::GitUnavailable(if detail.is_empty() {
                format!("{error}")
            } else {
                detail
            }));
        }
        return Err(error);
    }
    if !status.success() {
        return Err(AtlasError::GitUnavailable(detail));
    }
    // The child answered every object and exited cleanly; a write that
    // failed on the way in could only have shortened that answer, and it
    // did not.
    let _ = write_error;
    Ok(results)
}

pub(crate) fn blob_at_path(
    repo: &Path,
    commit: &str,
    wanted_path: &[u8],
) -> Result<Option<String>, AtlasError> {
    let raw = git(
        repo,
        &["ls-tree".into(), "-r".into(), "-z".into(), commit.into()],
    )?;
    for entry in raw
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            return Err(AtlasError::GitUnavailable(
                "invalid NUL-framed ls-tree record".into(),
            ));
        };
        if &entry[tab + 1..] != wanted_path {
            continue;
        }
        let meta = std::str::from_utf8(&entry[..tab])
            .map_err(|_| AtlasError::GitUnavailable("invalid Git metadata".into()))?;
        let fields: Vec<_> = meta.split_whitespace().collect();
        if fields.len() != 3 || fields[1] != "blob" {
            return Ok(None);
        }
        return Ok(Some(fields[2].to_owned()));
    }
    Ok(None)
}

pub(crate) fn tree_entries(
    repo: &Path,
    commit: &str,
) -> Result<Vec<(Vec<u8>, String, String)>, AtlasError> {
    let raw = git(
        repo,
        &["ls-tree".into(), "-r".into(), "-z".into(), commit.into()],
    )?;
    let mut entries = Vec::new();
    for entry in raw
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            return Err(AtlasError::GitUnavailable(
                "invalid NUL-framed ls-tree record".into(),
            ));
        };
        let meta = std::str::from_utf8(&entry[..tab])
            .map_err(|_| AtlasError::GitUnavailable("invalid Git metadata".into()))?;
        let fields: Vec<_> = meta.split_whitespace().collect();
        if fields.len() != 3 {
            return Err(AtlasError::GitUnavailable("invalid Git tree fields".into()));
        }
        entries.push((
            entry[tab + 1..].to_vec(),
            fields[0].to_owned(),
            fields[2].to_owned(),
        ));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

#[cfg(test)]
mod batched_read_tests {
    use super::*;
    use std::time::Duration;

    /// A checked-in stand-in for `git cat-file --batch-command`: static, and
    /// never written by the test process, so it cannot race the exec-of-a-
    /// freshly-written-file ETXTBSY failure (see the fixture's own header
    /// comment for the full explanation). Per-test behaviour is data: the
    /// `-C <repo>` directory git.rs already passes carries `stderr_bytes`
    /// (how much the stand-in writes to stderr before answering, which is
    /// how a child that talks past the pipe buffer behaves) and `body`
    /// (the answering script) as plain files, read by the fixture at run
    /// time rather than baked into an executable.
    fn stub(stderr_bytes: usize, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join(".stub-stderr-bytes"),
            stderr_bytes.to_string(),
        )
        .expect("stub stderr-bytes data");
        std::fs::write(dir.path().join(".stub-body.sh"), body).expect("stub body data");
        let fixture = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/atlas-git-stub.sh"
        ))
        .to_owned();
        (dir, fixture)
    }

    const ANSWER_ONE: &str = "while read -r cmd oid; do [ -z \"$oid\" ] && continue; printf '%s blob 5\\n' \"$oid\"; \
         printf 'hello\\n'; done";

    fn oids() -> Vec<String> {
        vec!["ce013625030ba8dba906f756967f9e9ca394464a".to_owned()]
    }

    /// Watched red before the drain thread existed: with 200_000 bytes on
    /// stderr and nothing emptying that pipe, the child blocks mid-write,
    /// its stdout stops, and this call never returns. The bound here is
    /// generous on purpose — this is a hang/no-hang test, not a timing one.
    #[test]
    fn a_child_that_talks_past_the_pipe_buffer_does_not_wedge_the_batched_read() {
        let (dir, program) = stub(200_000, ANSWER_ONE);
        let repo = dir.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(blobs_with_program(&program, &repo, &oids()));
        });
        let outcome = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the batched read did not return: a full stderr pipe wedged it");
        let blobs = outcome.expect("the stub answered every requested object");
        assert_eq!(
            blobs.get(&oids()[0]).map(Vec::as_slice),
            Some(b"hello".as_slice())
        );
    }

    /// The same shape on the failure side: a child that fails *and* is
    /// talkative about it must still be waited for, and what it said must
    /// still reach the caller as `GitUnavailable`, bounded rather than
    /// unbounded.
    #[test]
    fn a_talkative_failing_child_reports_its_own_account_bounded() {
        let (dir, program) = stub(200_000, "exit 128");
        let repo = dir.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(blobs_with_program(&program, &repo, &oids()));
        });
        let outcome = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the batched read did not return on a failing, talkative child");
        match outcome {
            Err(AtlasError::GitUnavailable(detail)) => {
                assert!(
                    detail.len() <= BATCH_STDERR_KEPT,
                    "kept {} bytes of a child's stderr; the bound is {BATCH_STDERR_KEPT}",
                    detail.len()
                );
                assert!(
                    detail.starts_with('w'),
                    "the child's own account is reported"
                );
            }
            other => panic!("expected GitUnavailable, got {other:?}"),
        }
    }

    /// A missing object is an omission from the map, never a hard failure:
    /// the same per-object tolerance `blob` gives a caller that reads one
    /// object at a time. Verified against the real `git`.
    #[test]
    fn a_missing_object_is_omitted_and_the_rest_still_arrive() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path();
        for argv in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "t"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(&argv)
                .status()
                .expect("git");
            assert!(status.success(), "git {argv:?}");
        }
        std::fs::write(repo.join("a.txt"), b"hello\n").expect("write");
        for argv in [vec!["add", "."], vec!["commit", "-qm", "x"]] {
            let status = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(&argv)
                .status()
                .expect("git");
            assert!(status.success(), "git {argv:?}");
        }
        let present = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["rev-parse", "HEAD:a.txt"])
                .output()
                .expect("git")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_owned();
        let absent = "0".repeat(40);
        // Deliberately including a duplicate and an empty object id: both
        // reach the real `git`, and both must leave the session's answers
        // aligned with the ids that were asked for.
        let asked = vec![
            absent.clone(),
            present.clone(),
            String::new(),
            present.clone(),
        ];
        let blobs = blobs(repo, &asked).expect("a missing object is not a session failure");
        assert_eq!(
            blobs.get(&present).map(Vec::as_slice),
            Some(b"hello\n".as_slice())
        );
        assert!(!blobs.contains_key(&absent));
        assert!(!blobs.contains_key(""));
        assert_eq!(blobs.len(), 1);
        // Every object `blob` returns, `blobs` returns the same bytes for.
        assert_eq!(blob(repo, &present).expect("blob"), blobs[&present]);
    }
}
