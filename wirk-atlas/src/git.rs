use crate::{
    AtlasError, ContentFamily, CoverageDisposition, ExtractorPolicy, GenerationId, ResourceRecord,
};
use std::path::Path;
use std::process::Command;

/// The acquisition policy label a Git generation records in
/// `SourceGeneration::acquisition_policy`. Named here, beside the code
/// that actually shells to `git`, rather than as a bare string literal
/// repeated at each call site (`store.rs`'s dispatch, `extract.rs`'s
/// `generation_id`) — parallel to `doctree::ACQUISITION_POLICY`.
pub(crate) const ACQUISITION_POLICY: &str = "git-tree-policy/v1";

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
        Err(AtlasError::SourceBytesUnavailable(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}
fn text(repo: &Path, args: &[String]) -> Result<String, AtlasError> {
    String::from_utf8(git(repo, args)?)
        .map(|s| s.trim().to_owned())
        .map_err(|_| AtlasError::SourceBytesUnavailable("non UTF-8 Git object identifier".into()))
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
        return Err(AtlasError::SourceBytesUnavailable(
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
/// One row of a `git ls-tree -r -z -l <commit>` listing, parsed exactly
/// once and reused by both `resources` (which classifies each entry all
/// the way to a `ResourceRecord`, reading a blob where admitted) and
/// `preview` (which classifies the same entry into a coverage bucket
/// without ever reading one) — one interpretation of the NUL/tab-framed
/// wire format, not two that could drift apart.
struct LsTreeEntry {
    path: Vec<u8>,
    mode: String,
    oid: String,
    size: Option<u64>,
}

fn parse_ls_tree(raw: &[u8]) -> Result<Vec<LsTreeEntry>, AtlasError> {
    let mut entries = Vec::new();
    for entry in raw.split(|b| *b == 0).filter(|x| !x.is_empty()) {
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            return Err(AtlasError::SourceBytesUnavailable(
                "invalid NUL-framed ls-tree record".into(),
            ));
        };
        let meta = std::str::from_utf8(&entry[..tab])
            .map_err(|_| AtlasError::SourceBytesUnavailable("invalid Git metadata".into()))?;
        let fields: Vec<_> = meta.split_whitespace().collect();
        if fields.len() != 4 {
            return Err(AtlasError::SourceBytesUnavailable(
                "invalid Git tree fields".into(),
            ));
        }
        entries.push(LsTreeEntry {
            path: entry[tab + 1..].to_vec(),
            mode: fields[0].to_owned(),
            oid: fields[2].to_owned(),
            size: fields[3].parse::<u64>().ok(),
        });
    }
    Ok(entries)
}

/// What a path/mode/size alone — no blob read — settles about one Git
/// entry, shared by `resources`' full classification and `preview`'s
/// coverage bucket. The one place either function decides
/// gitlink/symlink/excluded/no-extractor/oversize/family/candidate, so a
/// preview and a real acquisition can never quietly disagree about where
/// that boundary falls.
enum PathVerdict {
    Unsupported(&'static str),
    Excluded,
    /// Admitted by name; a real acquisition reads and extracts it.
    Family,
    /// The name settles nothing; a real acquisition screens a bounded
    /// prefix of the blob before deciding.
    Candidate,
}

fn classify_path(
    mode: &str,
    path: &[u8],
    size: Option<u64>,
    policy: &ExtractorPolicy,
    limits: &crate::doctree::CaptureLimits,
) -> PathVerdict {
    if mode == "160000" {
        return PathVerdict::Unsupported("gitlink");
    }
    if mode == "120000" {
        return PathVerdict::Unsupported("symlink");
    }
    if ExtractorPolicy::excluded(path) {
        return PathVerdict::Excluded;
    }
    match policy.admission(path) {
        crate::extract::PathAdmission::No => {
            PathVerdict::Unsupported("no extractor for path family")
        }
        // Checked from the size Git already reported, before the object
        // is read: the bound refuses the read rather than complaining
        // about one that already happened.
        _ if size.is_some_and(|size| limits.file_over(size)) => {
            PathVerdict::Unsupported("blob exceeds the bounded source read size")
        }
        crate::extract::PathAdmission::Family(_) => PathVerdict::Family,
        crate::extract::PathAdmission::Candidate => PathVerdict::Candidate,
    }
}

/// Every committed path of `commit`, classified and — where admitted —
/// extracted, under this estate's own source input bounds.
///
/// **The bounds are the operator's, where the operator set any.** Both
/// come from the estate's configurable `ResourcePolicy` and both default
/// to absent, so by default a repository's committed content is read as
/// committed. Where `limits.max_file_bytes` *is* configured, a larger
/// blob is reported `Unsupported` from the size `ls-tree -l` already
/// returned, without being read at all; where `limits.max_total_bytes`
/// is configured, the aggregate of everything actually read is charged
/// against it before each read it bounds. One blob is resident at a
/// time either way — read, extracted, dropped — so the aggregate
/// charge bounds the operator's own budget rather than this process's
/// residency.
///
/// **Two passes, because detection needs bytes and bounds do not.** The
/// first decides everything a path settles on its own. Paths the name
/// settles nothing about are screened together, in one batched read of at
/// most `document::SNIFF_BYTES` per object, and only those whose content
/// `anydoc` actually recognizes go on to a full read.
pub(crate) fn resources(
    repo: &Path,
    commit: &str,
    generation: &GenerationId,
    policy: &ExtractorPolicy,
    limits: &crate::doctree::CaptureLimits,
    sink: &mut dyn FnMut(ResourceRecord) -> Result<(), AtlasError>,
) -> Result<(), AtlasError> {
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

    enum Classified {
        /// Settled without reading anything.
        Decided(CoverageDisposition, Option<String>),
        /// Admitted by name; read and extract.
        Read,
        /// The name settles nothing; screen a bounded prefix first.
        Screen,
    }

    /// One `ls-tree` row, with everything its path alone already settled.
    struct Entry {
        path: Vec<u8>,
        mode: String,
        oid: String,
        size: Option<u64>,
        classified: Classified,
    }

    let mut entries: Vec<Entry> = Vec::new();
    for row in parse_ls_tree(&raw)? {
        let classified = match classify_path(&row.mode, &row.path, row.size, policy, limits) {
            PathVerdict::Unsupported(detail) => {
                Classified::Decided(CoverageDisposition::Unsupported, Some(detail.into()))
            }
            PathVerdict::Excluded => Classified::Decided(
                CoverageDisposition::Excluded,
                Some("fixed secret-like policy".into()),
            ),
            PathVerdict::Family => Classified::Read,
            PathVerdict::Candidate => Classified::Screen,
        };
        entries.push(Entry {
            path: row.path,
            mode: row.mode,
            oid: row.oid,
            size: row.size,
            classified,
        });
    }

    // **Screening is a read, and it is charged as one.** Git streams a
    // whole object across the batch pipe whatever the reader keeps, so
    // "look at the first kilobyte" costs the object's full length in real
    // I/O: keeping 1024 bytes in memory bounds the buffer, not the input.
    // A screen that was not charged would have let a tree of unrecognized
    // names move arbitrarily many bytes outside the budget that is
    // supposed to bound exactly that, and a screen-positive object would
    // then have been read a second time in full.
    //
    // So each candidate is read once, charged once, and its bytes are
    // reused: the same buffer that answers "is this a document?" is the
    // buffer extraction runs on. The per-file bound already refused
    // anything oversize from `ls-tree -l`'s own size, before this.
    //
    // Deduplication is kept where it can still save the work: a screen
    // decision is remembered per object id, so a second path naming an
    // object already screened negative is refused without reading it
    // again and without charging for it.
    // Sorted before the reads rather than after them, because records
    // now leave this function one at a time and their order is part of
    // what a generation records (`AtlasStore::validate_generation`
    // refuses a resource list that is not uniquely sorted, and
    // `publish_verify_git` zips it against `tree_entries`, which sorts
    // the same way).
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let mut screened: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    let mut total_bytes: u64 = 0;
    for Entry {
        path,
        mode,
        oid,
        size,
        classified,
    } in entries
    {
        let unsupported_family = || {
            (
                CoverageDisposition::Unsupported,
                Some("no extractor for path family".to_string()),
                vec![],
            )
        };
        let (disposition, detail, units) = match classified {
            Classified::Decided(disposition, detail) => (disposition, detail, vec![]),
            // Already screened negative under another path: no read, no
            // charge, same answer.
            Classified::Screen if screened.get(&oid) == Some(&false) => unsupported_family(),
            Classified::Read | Classified::Screen => {
                // The aggregate budget is charged before the read it
                // bounds, from the length Git already reported: a visible
                // refusal before the read, not an unbounded read followed
                // by a complaint. A candidate's screening read is charged
                // here too, because it is the same read.
                total_bytes = total_bytes.saturating_add(size.unwrap_or(0));
                if let Some(max_total) = limits.max_total_bytes
                    && total_bytes > max_total
                {
                    return Err(AtlasError::InvalidRequest(format!(
                        "git source reached {total_bytes} bytes, over the {max_total}-byte \
                         bounded aggregate read budget this estate configured for one capture; \
                         raise document_max_total_bytes in this estate's .wirk/resources.json to \
                         admit a larger tree"
                    )));
                }
                match git(repo, &["cat-file".into(), "blob".into(), oid.clone()]) {
                    // The screen, on the bytes this read already has.
                    Ok(bytes)
                        if matches!(classified, Classified::Screen) && {
                            let window = &bytes[..bytes.len().min(crate::document::SNIFF_BYTES)];
                            let could = crate::document::could_be_document(window);
                            screened.insert(oid.clone(), could);
                            !could
                        } =>
                    {
                        unsupported_family()
                    }
                    // An admitted document format's own bytes are expected
                    // to be binary, and `policy.units` reads them directly
                    // rather than as UTF-8, so the null-byte heuristic must
                    // not preempt it. Decided from the same bytes the
                    // family decision reads.
                    Ok(bytes)
                        if policy.family(&path, &bytes) != Some(ContentFamily::Document)
                            && bytes.contains(&0) =>
                    {
                        (
                            CoverageDisposition::Unsupported,
                            Some("binary blob".into()),
                            vec![],
                        )
                    }
                    Ok(bytes) => match policy.units(generation, &path, &oid, &bytes) {
                        Ok(units) => (CoverageDisposition::Indexed, None, units),
                        Err(detail) => (CoverageDisposition::Error, Some(detail), vec![]),
                    },
                    Err(AtlasError::SourceBytesUnavailable(detail)) => {
                        (CoverageDisposition::Unavailable, Some(detail), vec![])
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        // Handed over and dropped here: one blob's bytes and one
        // record's units at a time, never the whole tree's.
        sink(ResourceRecord {
            path,
            mode,
            object_id: Some(oid),
            byte_len: size,
            disposition,
            detail,
            units,
        })?;
    }
    Ok(())
}
/// `atlas acquire --dry-run`'s Git half: the same `git ls-tree -l`
/// listing `resources` reads, classified by path/mode/size alone — no
/// blob is ever read here, so this is strictly cheaper than
/// `resources`, which this never calls and which alone actually
/// extracts anything.
///
/// A path `resources` would itself read without sniffing first
/// (`PathAdmission::Family`) is reported `candidate`: whether it
/// extracts is not knowable without that read, which this deliberately
/// skips. A path `resources` would itself sniff before deciding
/// (`PathAdmission::Candidate`) is reported `unclassified` rather than
/// guessed at — Git's preview trades that one honest gap for reading
/// zero bytes of tracked content, which is the entire reason a
/// pre-acquisition preview over a large Git tree stays cheap.
pub(crate) fn preview(
    repo: &Path,
    commit: &str,
    policy: &ExtractorPolicy,
    limits: &crate::doctree::CaptureLimits,
) -> Result<crate::preview::PreviewReport, AtlasError> {
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
    let mut report = crate::preview::PreviewReport::new(
        "git",
        repo.display().to_string(),
        commit.to_string(),
        false,
    );
    for row in parse_ls_tree(&raw)? {
        let size = row.size.unwrap_or(0);
        match classify_path(&row.mode, &row.path, row.size, policy, limits) {
            PathVerdict::Unsupported(_) => report.unsupported.add(size),
            PathVerdict::Excluded => report.excluded.add(size),
            PathVerdict::Family => report.candidate.add(size),
            PathVerdict::Candidate => report.unclassified.add(size),
        }
    }
    Ok(report)
}

pub(crate) fn blob(repo: &Path, oid: &str) -> Result<Vec<u8>, AtlasError> {
    git(repo, &["cat-file".into(), "blob".into(), oid.into()])
}

/// Same committed bytes as `blob`, one object at a time, but over a
/// single long-lived `git cat-file --batch-command` session instead of
/// one process spawn per object. Reads exactly the objects named by
/// `oids`, in order, and returns exactly what `blob` would have returned
/// for each: on a missing or otherwise unreadable object this fails with
/// the same `AtlasError::SourceBytesUnavailable` shape a lone `blob(repo, oid)`
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
                read_error = Some(AtlasError::SourceBytesUnavailable(format!(
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
            read_error = Some(AtlasError::SourceBytesUnavailable(format!(
                "malformed git cat-file --batch-command header for {oid}: {header:?}"
            )));
            break;
        };
        let Ok(size) = size.parse::<usize>() else {
            read_error = Some(AtlasError::SourceBytesUnavailable(format!(
                "non-numeric object size from git cat-file --batch-command for {oid}"
            )));
            break;
        };
        if got != oid {
            read_error = Some(AtlasError::SourceBytesUnavailable(format!(
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
        // `AtlasError::SourceBytesUnavailable(stderr)` a lone `blob(repo, oid)`
        // call against that repo would already have produced, so a
        // caller that tolerates "this source is unavailable" continues
        // to see that, not a new, narrower failure shape.
        if !status.success() {
            return Err(AtlasError::SourceBytesUnavailable(if detail.is_empty() {
                format!("{error}")
            } else {
                detail
            }));
        }
        return Err(error);
    }
    if !status.success() {
        return Err(AtlasError::SourceBytesUnavailable(detail));
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
            return Err(AtlasError::SourceBytesUnavailable(
                "invalid NUL-framed ls-tree record".into(),
            ));
        };
        if &entry[tab + 1..] != wanted_path {
            continue;
        }
        let meta = std::str::from_utf8(&entry[..tab])
            .map_err(|_| AtlasError::SourceBytesUnavailable("invalid Git metadata".into()))?;
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
            return Err(AtlasError::SourceBytesUnavailable(
                "invalid NUL-framed ls-tree record".into(),
            ));
        };
        let meta = std::str::from_utf8(&entry[..tab])
            .map_err(|_| AtlasError::SourceBytesUnavailable("invalid Git metadata".into()))?;
        let fields: Vec<_> = meta.split_whitespace().collect();
        if fields.len() != 3 {
            return Err(AtlasError::SourceBytesUnavailable(
                "invalid Git tree fields".into(),
            ));
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
    /// still reach the caller as `SourceBytesUnavailable`, bounded rather than
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
            Err(AtlasError::SourceBytesUnavailable(detail)) => {
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
            other => panic!("expected SourceBytesUnavailable, got {other:?}"),
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

    /// One committed repository whose files are named by `files`, with its
    /// own Git identity so the commit does not depend on this host's.
    fn commit_repo(files: &[(&str, &[u8])]) -> (tempfile::TempDir, String) {
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
        for (name, bytes) in files {
            std::fs::write(repo.join(name), bytes).expect("write");
        }
        for argv in [vec!["add", "-A"], vec!["commit", "-qm", "x"]] {
            let status = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(&argv)
                .status()
                .expect("git");
            assert!(status.success(), "git {argv:?}");
        }
        let head = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("git")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_owned();
        (dir, head)
    }

    /// `resources` streams its records to a sink; a check that wants the
    /// whole list collects them here, which is exactly what production
    /// no longer does.
    fn resources_to_vec(
        repo: &Path,
        commit: &str,
        generation: &GenerationId,
        policy: &ExtractorPolicy,
        limits: &crate::doctree::CaptureLimits,
    ) -> Result<Vec<ResourceRecord>, AtlasError> {
        let mut records = Vec::new();
        resources(repo, commit, generation, policy, limits, &mut |record| {
            records.push(record);
            Ok(())
        })?;
        Ok(records)
    }

    fn record<'a>(records: &'a [ResourceRecord], name: &str) -> &'a ResourceRecord {
        records
            .iter()
            .find(|record| record.path == name.as_bytes())
            .unwrap_or_else(|| panic!("no record for {name}"))
    }

    /// A Git source is bounded by this estate's own configurable source
    /// input bounds, not by whatever the extractor happens to refuse once
    /// the bytes are already in memory. The per-file bound is decided from
    /// the size `ls-tree -l` reports, so an oversize blob is named without
    /// being read, and raising the bound indexes it.
    #[test]
    fn a_blob_over_the_per_file_bound_is_named_then_admitted_when_raised() {
        let big = vec![b'x'; 40_000];
        let (dir, head) = commit_repo(&[("big.md", &big), ("small.md", b"# small\n")]);
        let policy = ExtractorPolicy::default();
        let generation = GenerationId("g-test".into());

        let tight = crate::doctree::CaptureLimits {
            max_file_bytes: Some(4_096),
            ..Default::default()
        };
        let records =
            resources_to_vec(dir.path(), &head, &generation, &policy, &tight).expect("capture");
        let refused = record(&records, "big.md");
        assert_eq!(refused.disposition, CoverageDisposition::Unsupported);
        assert_eq!(
            refused.detail.as_deref(),
            Some("blob exceeds the bounded source read size")
        );
        assert_eq!(
            record(&records, "small.md").disposition,
            CoverageDisposition::Indexed
        );

        let mut raised = tight;
        raised.max_file_bytes = Some(1_000_000);
        let records =
            resources_to_vec(dir.path(), &head, &generation, &policy, &raised).expect("capture");
        assert_eq!(
            record(&records, "big.md").disposition,
            CoverageDisposition::Indexed
        );
    }

    /// The aggregate half of the same bound: a whole capture that would
    /// read past it is refused visibly, naming the setting an operator
    /// raises, rather than read and complained about afterwards.
    #[test]
    fn the_aggregate_budget_refuses_the_whole_git_capture_visibly() {
        let body = vec![b'x'; 4_000];
        let (dir, head) = commit_repo(&[("a.md", &body), ("b.md", &body), ("c.md", &body)]);
        let policy = ExtractorPolicy::default();
        let generation = GenerationId("g-test".into());
        let tight = crate::doctree::CaptureLimits {
            max_total_bytes: Some(6_000),
            ..Default::default()
        };
        match resources_to_vec(dir.path(), &head, &generation, &policy, &tight) {
            Err(AtlasError::InvalidRequest(detail)) => {
                assert!(detail.contains("document_max_total_bytes"), "{detail}");
            }
            other => panic!("expected a visible aggregate refusal, got {other:?}"),
        }
    }

    /// Screening is real input, and the bound counts it.
    ///
    /// Git streams a whole object across the batch pipe however few of its
    /// bytes a reader keeps, so screening two unrecognized names costs
    /// their full length. A screen charged only for what it kept would
    /// have let a tree of such names move arbitrarily many bytes outside
    /// the budget that exists to bound exactly that.
    #[test]
    fn screening_an_unrecognized_name_is_charged_against_the_aggregate_budget() {
        let body = vec![b'x'; 4_000];
        // Two distinct bodies, so neither can be deduplicated away, and
        // neither is a document: their bytes are only ever read to find
        // that out.
        let mut other = body.clone();
        other[0] = b'y';
        let (dir, head) = commit_repo(&[("one.unknown", &body), ("two.unknown", &other)]);
        let policy = ExtractorPolicy::default();
        let generation = GenerationId("g-test".into());
        let tight = crate::doctree::CaptureLimits {
            max_total_bytes: Some(6_000),
            ..Default::default()
        };
        match resources_to_vec(dir.path(), &head, &generation, &policy, &tight) {
            Err(AtlasError::InvalidRequest(detail)) => {
                assert!(detail.contains("document_max_total_bytes"), "{detail}");
            }
            other => panic!("the bytes a screen actually reads must be charged; got {other:?}"),
        }

        // Raised past what the screening reads really cost, the same tree
        // is admitted -- and still reported unsupported, because neither
        // body is a document.
        let raised = crate::doctree::CaptureLimits {
            max_total_bytes: Some(100_000),
            ..Default::default()
        };
        let records =
            resources_to_vec(dir.path(), &head, &generation, &policy, &raised).expect("capture");
        for name in ["one.unknown", "two.unknown"] {
            assert_eq!(
                record(&records, name).disposition,
                CoverageDisposition::Unsupported,
                "{name}"
            );
        }
    }

    /// One object screened once. Two unrecognized names over the identical
    /// blob are one read and one charge, not two: the second is answered
    /// from the decision the first produced.
    #[test]
    fn a_screen_decision_is_reused_across_paths_sharing_one_object() {
        let body = vec![b'x'; 4_000];
        let (dir, head) = commit_repo(&[("one.unknown", &body), ("two.unknown", &body)]);
        let policy = ExtractorPolicy::default();
        let generation = GenerationId("g-test".into());
        // Room for exactly one of the two reads. Reading the same object
        // twice would exceed it.
        let tight = crate::doctree::CaptureLimits {
            max_total_bytes: Some(6_000),
            ..Default::default()
        };
        let records = resources_to_vec(dir.path(), &head, &generation, &policy, &tight)
            .expect("one object is read and charged once");
        for name in ["one.unknown", "two.unknown"] {
            let found = record(&records, name);
            assert_eq!(
                found.disposition,
                CoverageDisposition::Unsupported,
                "{name}"
            );
            assert_eq!(
                found.detail.as_deref(),
                Some("no extractor for path family"),
                "{name}"
            );
        }
    }

    /// A candidate that really is a document is read once, not twice: the
    /// bytes the screen looked at are the bytes extraction runs on. A
    /// second full read would charge its length again, so the budget is
    /// what proves it.
    #[test]
    fn a_screen_positive_candidate_is_not_read_a_second_time() {
        let rtf = b"{\\rtf1\\ansi\\deff0 {\\fonttbl{\\f0 Times;}}\\f0 Chargemarker prose.\\par}";
        let (dir, head) = commit_repo(&[("brief", rtf.as_slice())]);
        let policy = ExtractorPolicy::default();
        let generation = GenerationId("g-test".into());
        // Enough for one read of this blob, not two.
        let tight = crate::doctree::CaptureLimits {
            max_total_bytes: Some((rtf.len() as u64) + 1),
            ..Default::default()
        };
        let records = resources_to_vec(dir.path(), &head, &generation, &policy, &tight)
            .expect("a screened-and-extracted blob is charged once");
        let found = record(&records, "brief");
        assert_eq!(
            found.disposition,
            CoverageDisposition::Indexed,
            "{:?}",
            found.detail
        );
    }

    /// A committed document whose name settles nothing is screened on a
    /// bounded prefix and then admitted for what its content is, while an
    /// unrecognized name over ordinary bytes stays unsupported.
    #[test]
    fn a_committed_document_under_an_unrecognized_name_is_detected() {
        let rtf = b"{\\rtf1\\ansi\\deff0 {\\fonttbl{\\f0 Times;}}\\f0 Gitmarker prose.\\par}";
        let (dir, head) = commit_repo(&[
            ("brief", rtf.as_slice()),
            ("payload.unknown", b"just prose, no container\n"),
        ]);
        let policy = ExtractorPolicy::default();
        let generation = GenerationId("g-test".into());
        let limits = crate::doctree::CaptureLimits::default();
        let records =
            resources_to_vec(dir.path(), &head, &generation, &policy, &limits).expect("capture");

        let detected = record(&records, "brief");
        assert_eq!(
            detected.disposition,
            CoverageDisposition::Indexed,
            "{:?}",
            detected.detail
        );
        assert_eq!(
            detected.units.first().map(|unit| unit.family),
            Some(ContentFamily::Document)
        );
        let plain = record(&records, "payload.unknown");
        assert_eq!(plain.disposition, CoverageDisposition::Unsupported);
        assert_eq!(
            plain.detail.as_deref(),
            Some("no extractor for path family")
        );
    }
}
