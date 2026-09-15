//! A third acquisition policy beside `git`'s and `doctree`'s: one
//! explicitly admitted public HTTP(S) URL, fetched through the host's
//! own installed `curl` — never a hand-rolled TLS/HTTP client and never
//! a shell (every argument reaches `curl` as its own argv element, so
//! nothing in a URL is ever shell-interpreted).
//!
//! **What this policy observes, and what it deliberately does not
//! invent.** A document tree has one observable state, its current one
//! (`doctree::CURRENT_OBSERVATION`); an HTTP source has exactly the
//! same shape of single observation — the bytes an explicit
//! `acquire`/`refresh` actually read back, last time it was asked. Its
//! `revision`/`content` are the SHA-256 of those bytes, content-derived
//! like `doctree`'s, never an upstream "Last-Modified" or a page's own
//! displayed version string presented as though it were a commit. Where
//! the origin server discloses `ETag`/`Last-Modified`, this policy
//! records them verbatim on [`crate::HttpOrigin`] as *evidence a caller
//! can read*, not as the identity a coordinate resolves against —
//! disclosure, never a fabricated timeline.
//!
//! **What is cached, and why that is not an archive.** Unlike a Git
//! object store or a live local filesystem, there is nothing to read
//! "live" from a remote URL between one fetch and the next — re-reading
//! would mean a second network round trip, and `search`/`resolve` must
//! never reach the network (BUILD-BRIEF.md; this module's own
//! `capture` is the only thing here that does). So the fetched bytes
//! are persisted exactly once, inside the one place every other
//! policy's generation already lives immutably —
//! `AtlasLayout::generations/<id>/content.bin`, written by
//! `AtlasStore::stage` beside `manifest.json`/`resources.ndjson` — and
//! reclaimed through the exact same `wirk estate clean
//! --class atlas-generations` path every other orphaned generation
//! already is. No second, duplicate archive and no per-Work copy.

use crate::domain::HttpOrigin;
use crate::{AtlasError, CoverageDisposition, ExtractorPolicy, GenerationId, ResourceRecord};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use ulid::Ulid;

/// The acquisition policy label an HTTP generation records in
/// `SourceGeneration::acquisition_policy`, parallel to
/// `git::ACQUISITION_POLICY`/`doctree::ACQUISITION_POLICY`.
pub const ACQUISITION_POLICY: &str = "http-source-policy/v1";

/// The only `requested_ref`/`--revision` spelling this policy honours,
/// for the same reason `doctree::CURRENT_OBSERVATION` exists: an HTTP
/// source has no revision besides the state its own last fetch actually
/// observed, and any other caller-supplied string would later be read
/// back as if it named something this policy had checked against.
pub const CURRENT_OBSERVATION: &str = "current";

/// What one fetch is bounded by, resolved from the estate's own
/// resource policy — `wirk_core::jobs::ResourcePolicy`'s
/// `http_max_response_bytes`/`http_timeout_secs`/`http_max_redirects` —
/// never a constant fixed in this module.
///
/// Every field is optional and absent by default: an explicitly
/// admitted URL is fetched as the resource it is, and each bound is
/// applied only where an operator wrote one. Absence is expressed to
/// `curl` by not passing the option at all (or, for redirects, by its
/// own documented "unlimited"), never by a large number standing in for
/// no number.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FetchLimits {
    pub(crate) max_response_bytes: Option<u64>,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) max_redirects: Option<u32>,
}

impl FetchLimits {
    pub(crate) fn from_policy(policy: &wirk_core::jobs::ResourcePolicy) -> Self {
        Self {
            max_response_bytes: policy.http_max_response_bytes,
            timeout_secs: policy.http_timeout_secs,
            max_redirects: policy.http_max_redirects,
        }
    }
}

/// Whatever the estate's own `ResourcePolicy` defaults to, for the same
/// reason `doctree::CaptureLimits`'s does: a check about "the default"
/// has to read the default that production reads.
impl Default for FetchLimits {
    fn default() -> Self {
        Self::from_policy(&wirk_core::jobs::ResourcePolicy::default())
    }
}

/// The first `curl` release whose `--max-filesize` aborts a transfer the
/// origin never declared a length for. Before it, the option only refused
/// a response whose `Content-Length` already said it was too large, and a
/// chunked or length-less body streamed to completion regardless — so on
/// an older `curl` the estate's own `http_max_response_bytes` is not
/// something this policy can honestly say it enforces.
const REQUIRED_CURL: (u32, u32, u32) = (8, 4, 0);

/// The `major.minor.patch` `curl --version` reports on its first line,
/// which is `curl <version> (<host>) ...`. `None` when that line is not
/// the shape `curl` documents; the caller treats that as "cannot
/// establish the capability", never as "the capability is present".
fn parse_curl_version(first_line: &str) -> Option<(u32, u32, u32)> {
    let version = first_line
        .strip_prefix("curl ")?
        .split_whitespace()
        .next()?;
    let numeric: String = version
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = numeric.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let patch = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Some((major, minor, patch))
}

/// Whether an installed `curl` of `version` can enforce a byte bound on a
/// response whose length the origin never declares.
fn enforces_unknown_length_bound(version: (u32, u32, u32)) -> bool {
    version >= REQUIRED_CURL
}

/// What this host's own `curl` is, asked before any network access —
/// and asked **only when this estate configured a response-size bound**.
///
/// The bound an operator configured has to be one this policy can
/// actually apply, and on an older `curl` it is not: a chunked body would
/// stream past `http_max_response_bytes` and only be refused after its
/// bytes were already staged. Rather than advertise an enforcement that
/// is not there, this refuses the acquisition with the version it found,
/// the version it needs, and what the operator can do about it.
///
/// Where no such bound is set there is no enforcement to be unable to
/// deliver, so this check is not a precondition for fetching at all.
/// Making it one would have turned a capability needed for one optional
/// setting into a reason to refuse every HTTP source on the host — a
/// refusal of admitted work that nothing had asked for. `-q` is first
/// here for the same reason it is first in the fetch itself.
fn check_curl_capability() -> Result<(), AtlasError> {
    let output = Command::new("curl")
        .env_clear()
        .arg("-q")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| {
            AtlasError::SourceBytesUnavailable(format!(
                "this host's curl could not be run, so no HTTP source can be acquired: {error}"
            ))
        })?;
    let banner = String::from_utf8_lossy(&output.stdout);
    let first_line = banner.lines().next().unwrap_or("").trim();
    let found = parse_curl_version(first_line);
    match found {
        Some(version) if enforces_unknown_length_bound(version) => Ok(()),
        Some((major, minor, patch)) => {
            let (want_major, want_minor, want_patch) = REQUIRED_CURL;
            Err(AtlasError::InvalidRequest(format!(
                "this host's curl is {major}.{minor}.{patch}. curl \
                 {want_major}.{want_minor}.{want_patch} is the first release whose \
                 --max-filesize aborts a response the origin declares no length for, so this \
                 estate's http_max_response_bytes cannot be enforced on a chunked body here. \
                 Install curl {want_major}.{want_minor}.{want_patch} or newer to acquire HTTP \
                 sources."
            )))
        }
        None => Err(AtlasError::SourceBytesUnavailable(format!(
            "this host's curl did not report a version this policy can read ({first_line:?}), so \
             it cannot be shown to enforce a bound on a response of undeclared length"
        ))),
    }
}

pub(crate) fn hash_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Refuses everything but a plain `http`/`https` locator with no
/// embedded userinfo (`user:pass@host`, a credential-bearing locator
/// this policy will not acquire, log, or record) and no `..` path
/// traversal segment. Checked at registration and again at every
/// acquire/refresh, so a membership's own recorded locator is re-
/// validated rather than trusted forever.
pub(crate) fn validate_url(url: &str) -> Result<(), AtlasError> {
    let Some(scheme_end) = url.find("://") else {
        return Err(AtlasError::InvalidRequest(format!(
            "{url:?} is not an http(s) URL: no \"scheme://\" prefix"
        )));
    };
    let scheme = &url[..scheme_end];
    if scheme != "http" && scheme != "https" {
        return Err(AtlasError::InvalidRequest(format!(
            "{url:?} names an unsupported protocol {scheme:?}; only \"http\" and \"https\" are \
             admitted"
        )));
    }
    let rest = &url[scheme_end + 3..];
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err(AtlasError::InvalidRequest(format!("{url:?} names no host")));
    }
    if authority.contains('@') {
        return Err(AtlasError::InvalidRequest(
            "URL carries embedded userinfo (a credential-bearing locator); this policy admits \
             only a plain public URL and never acquires, logs, or records one carrying a \
             credential"
                .into(),
        ));
    }
    if url.split('/').any(|segment| segment == "..") {
        return Err(AtlasError::InvalidRequest(
            "URL path contains a \"..\" segment".into(),
        ));
    }
    Ok(())
}

/// The relative resource path one fetch is recorded under: the URL's
/// own path, stripped of query/fragment, `"index"` where it names none
/// (a bare host or a directory-shaped URL) — with a synthetic extension
/// appended **only** when the URL's own path carries no extension this
/// extractor edition already recognises, decided from the server's own
/// disclosed `Content-Type` rather than guessed. A URL that already
/// names a recognised extension keeps it untouched; a response whose
/// type this fetch cannot place stays extension-less and is reported
/// `Unsupported` by `capture`'s caller, honestly, rather than forced
/// into a family it was never shown to be.
fn resource_path(url: &str, policy: &ExtractorPolicy, content_type: Option<&str>) -> Vec<u8> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path_part = after_scheme.split_once('/').map_or("", |(_, rest)| rest);
    let path_part = path_part
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_start_matches('/');
    let mut name = if path_part.is_empty() || path_part.ends_with('/') {
        format!("{path_part}index")
    } else {
        path_part.to_string()
    };
    if matches!(
        policy.admission(name.as_bytes()),
        crate::extract::PathAdmission::Candidate | crate::extract::PathAdmission::No
    ) && let Some(content_type) = content_type
    {
        let content_type = content_type.to_ascii_lowercase();
        // Only what the origin's own declared type names, and only where
        // the URL named nothing. A type this fetch cannot place leaves the
        // path as it is; content detection still gets its turn on the
        // bytes, and a response neither can place is reported
        // `Unsupported` honestly rather than forced into a family.
        for (marker, extension) in [
            ("html", ".html"),
            ("markdown", ".md"),
            // Delimited text carries no signature to detect, so the
            // origin's own declared type is the only thing that can name
            // it.
            ("csv", ".csv"),
        ] {
            if content_type.contains(marker) {
                name.push_str(extension);
                break;
            }
        }
    }
    name.into_bytes()
}

/// The last (post-redirect) response header block `-D` recorded, parsed
/// into lowercase-keyed `(name, value)` pairs. `-L` makes `curl` follow
/// redirects and append every hop's own header block to the same file
/// in order, so only the final block — the response this fetch actually
/// used — is read here; an intermediate `301`'s headers are not this
/// fetch's origin.
fn last_header_block(text: &str) -> Vec<(String, String)> {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.starts_with("HTTP/") && !current.is_empty() {
            blocks.push(std::mem::take(&mut current));
        }
        current.push(line);
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    let Some(last) = blocks.pop() else {
        return Vec::new();
    };
    last.into_iter()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect()
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// What is left of an explicit whole-chain transfer budget, in the only
/// resolution the transport actually has.
///
/// `curl --max-time` takes a decimal, but libcurl's own control behind
/// it is `CURLOPT_TIMEOUT_MS` — milliseconds — and its documentation
/// states that zero means *no* transfer timeout. So a remainder of, say,
/// 400µs has no positive representable form here: rendering it rounds to
/// `0.000`, which would hand `curl` the disabling value and run the hop
/// that carries the response body with the operator's explicit budget
/// not in force. That is the same inversion [`capture`] already refuses
/// for a written `http_timeout_secs: 0`, arrived at by arithmetic
/// instead of by configuration.
///
/// This type is how that cannot happen: it is constructible only from a
/// remainder that has a positive millisecond form, it truncates rather
/// than rounds (so a budget is never silently extended past what the
/// operator wrote), and it is the only thing `fetch_args` will render a
/// `--max-time` from. When [`Self::from_remaining`] answers `None` the
/// budget is spent, and the caller takes the exhausted-budget refusal it
/// already had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferBudget {
    millis: u64,
}

impl TransferBudget {
    pub(crate) fn from_remaining(remaining: std::time::Duration) -> Option<Self> {
        let millis = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
        (millis > 0).then_some(Self { millis })
    }

    /// The `--max-time` value, rendered from whole milliseconds so it can
    /// never be `0.000` and never exceeds the remainder it came from.
    fn max_time_arg(self) -> String {
        format!("{}.{:03}", self.millis / 1000, self.millis % 1000)
    }
}

/// What is left of `budget` once `elapsed` has been spent, as something
/// the transport can actually be given.
///
/// Separated from [`capture`]'s loop so the accounting is checkable
/// without a clock: the whole-chain rule is that each hop receives the
/// remainder rather than a fresh copy, and that is arithmetic, not
/// timing. `Ok(None)` is "no budget configured"; `Err(())` is "spent, or
/// too little left to express", which the caller reports as the
/// exhausted budget it is.
#[allow(clippy::result_unit_err)]
pub(crate) fn remaining_budget(
    budget: Option<std::time::Duration>,
    elapsed: std::time::Duration,
) -> Result<Option<TransferBudget>, ()> {
    let Some(budget) = budget else {
        return Ok(None);
    };
    match TransferBudget::from_remaining(budget.saturating_sub(elapsed)) {
        Some(left) => Ok(Some(left)),
        None => Err(()),
    }
}

/// Exactly the argument vector **one redirect hop** hands `curl`, in
/// order, so the controls whose correctness is positional can be checked
/// without a network.
///
/// **`-q` is first, and that is a requirement, not a style.** It is what
/// stops `curl` reading a configuration file, and it only has that effect
/// as the first argument. `env_clear` does not substitute for it: `curl`'s
/// own documented config search reaches the user's home directory through
/// `getpwuid`, with no environment variable in the path. An independent
/// measurement against a fixture `.curlrc` saw it both set a request
/// header and divert the response body away from the staging file this
/// policy reads back, under an otherwise cleared environment.
///
/// **`--globoff` is what keeps one admitted URL one transfer.** Without
/// it `curl` expands `{}`/`[]` into several requests — `--` does not stop
/// it — and the observed result was two real requests, the 404 body of a
/// URL that was never admitted persisted as content, and a `-w` line per
/// transfer whose first field was read as the whole answer. With it, a
/// literal bracket in an ordinary URL is also just part of the URL.
///
/// **`-L` is deliberately absent, and that is what makes a redirect
/// cycle nameable.** `curl -L` follows the chain itself and reports only
/// where it ended up; a chain that returns to a URL it has already
/// served is then indistinguishable, from outside, from a chain that is
/// merely long. Measured against a real two-URL cycle on `curl 8.18.0`,
/// `-L --max-redirs -1` issued about five thousand requests to the
/// origin and then exited 100 with "Too many response headers, 5000 is
/// max" — a guard inside the transport, arriving as a fault with the
/// operator's actual situation nowhere in it. So this asks `curl` for
/// one hop at a time and reads
/// `%{redirect_url}` — the curl manual's own
/// "when an HTTP request was made without --location to follow redirects
/// ... this variable shows the actual URL a redirect would have gone
/// to", `https://curl.se/docs/manpage.html#write-out` — as the
/// destination. `curl` resolves a relative `Location` against the
/// current URL itself, so nothing here parses a header or builds a URL
/// (R5: the installed tool's own resolution, not a second HTTP stack).
/// `--proto` still confines every hop to `http`/`https`, and
/// [`capture`] re-runs [`validate_url`] on each destination before
/// requesting it, which is strictly narrower than `--proto-redir` was:
/// a redirect to a credential-bearing locator is refused rather than
/// followed.
fn fetch_args(
    url: &str,
    limits: &FetchLimits,
    remaining: Option<TransferBudget>,
    header_path: &Path,
    body_path: &Path,
) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |value: &str| args.push(OsString::from(value));
    push("-q");
    push("--globoff");
    push("-sS");
    push("--proto");
    push("=http,https");
    // The two bounds below are passed only where the estate set them.
    // An option carrying a value nobody chose would be this product
    // deciding how big a resource may be or how long a transfer may
    // take, which is the thing being removed.
    //
    // `remaining` is what is left of an explicit `http_timeout_secs`
    // *across the whole chain*, not a fresh clock per hop: an operator
    // who writes one number means the fetch, and a per-hop copy would
    // silently multiply it by the chain's length. It is a
    // [`TransferBudget`] and not a `Duration` precisely so that a
    // remainder with no positive millisecond form cannot reach this
    // point and be rendered as `curl`'s disabling `0.000`.
    if let Some(remaining) = remaining {
        push("--max-time");
        push(&remaining.max_time_arg());
    }
    // `--max-filesize 0` is documented to *disable* the limit ("Setting
    // the maximum value to zero disables the limit"), so a written 0 —
    // which means "admit no body larger than nothing" — cannot be
    // expressed to `curl` and is enforced by this policy's own check on
    // the staged body instead. Every non-zero bound is `curl`'s to
    // enforce, on the transfer, before these bytes are ever read back.
    if let Some(max_bytes) = limits.max_response_bytes.filter(|max| *max > 0) {
        push("--max-filesize");
        push(&max_bytes.to_string());
    }
    push("-D");
    args.push(header_path.into());
    args.push(OsString::from("-o"));
    args.push(body_path.into());
    args.push(OsString::from("-w"));
    // One transfer, so one line. Tab-separated because a URL never
    // carries a literal tab, while `%{redirect_url}` is empty for a
    // final response and an empty field has to stay distinguishable.
    args.push(OsString::from(
        "%{http_code}\t%{url_effective}\t%{redirect_url}",
    ));
    args.push(OsString::from("--"));
    args.push(OsString::from(url));
    args
}

/// The staged result of one completed fetch: the response body still on
/// disk, where `curl` wrote it, plus everything this policy derived from
/// it without holding it in memory.
///
/// The body is deliberately **not** a `Vec<u8>`. `curl` streams the
/// response to a file; reading that file whole just to hash it and hand
/// it back made the response resident for the entire staging pipeline,
/// which is the one acquisition path ruling 0403's finding F3 names.
/// The digest here is folded from the file through one reused buffer,
/// and the caller streams the same file into the generation's
/// `content.bin`. The one place the whole body genuinely has to exist at
/// once is extraction, because `anydoc::to_markdown_bytes` and the text
/// unitizer both take `&[u8]` — a real interface constraint of the
/// adopted API, not a general licence to buffer.
///
/// Dropping this removes the staging directory, so an interrupted or
/// refused acquisition never leaves a response body behind.
pub(crate) struct Fetched {
    pub(crate) revision: String,
    pub(crate) content: String,
    pub(crate) origin: HttpOrigin,
    staging: PathBuf,
    body: PathBuf,
}

impl Fetched {
    pub(crate) fn body(&self) -> &Path {
        &self.body
    }
}

impl Drop for Fetched {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.staging);
    }
}

/// Removes a staging directory on every path that does not return a
/// [`Fetched`] (which owns its own cleanup).
struct StagingGuard {
    dir: PathBuf,
    kept: bool,
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if !self.kept {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// The SHA-256 of a file's bytes, folded through one reused 64 KiB
/// buffer rather than one allocation the size of the file.
fn hash_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// What one hop's `-w` line said.
struct HopOutcome {
    status: u16,
    /// `%{redirect_url}`: where a redirect this fetch did not follow
    /// points, or `None` for a response that is not a redirect.
    redirect: Option<String>,
}

/// One bounded fetch of `url`, run through this host's own installed
/// `curl` under `jobs`'s existing bounded-child containment (P4.5 B2):
/// process group, per-job cgroup where available, a wall-clock deadline
/// and a registered cancel token an operator's `atlas cancel --source`
/// reaches, exactly as `semantic.rs`'s backend invocation already gets.
/// Where this estate configured a bound, `curl` itself enforces it —
/// `--max-filesize` (exit 63), `--max-time` — so it is bounded IO before
/// this crate reads a single byte back, not a cap applied after the
/// read.
///
/// **Redirects are followed one hop at a time, and a repeated
/// destination is a cycle** (ruling 0403). A finite chain of any length
/// the origin actually serves is followed to its end; a chain that
/// points back at a URL this fetch has already requested is refused by
/// name, deterministically, instead of running until someone cancels it.
/// That is the exact condition this can prove and the only one it
/// claims: a server that emits an unbounded sequence of *distinct*
/// URLs, or one that stalls mid-body, is still non-terminating, and is
/// still ended the way every other non-terminating transfer is — by the
/// operator's `atlas cancel --source`, or by an explicit
/// `http_timeout_secs` where the estate set one. An explicit
/// `http_max_redirects` is applied to the hop count across the chain,
/// and an explicit `http_timeout_secs` bounds the chain as a whole
/// rather than each hop separately.
///
/// Every hop is re-checked by [`validate_url`], so the protocol and
/// no-credential restrictions that admitted the first URL also admit
/// each destination, and the header block read at the end is the final
/// response's own.
///
/// Returns the manifest identity (bare 64-hex SHA-256 `revision`, the
/// same value `"sha256:"`-tagged as `content` — this policy's whole
/// replacement for `git commit`/`tree`, parallel to `doctree`'s), the
/// disclosed [`HttpOrigin`], and the staged body on disk.
pub(crate) fn capture(
    verb: &str,
    url: &str,
    limits: &FetchLimits,
    staging_root: &Path,
    jobs: &crate::store::JobContext,
    scope: &str,
) -> Result<Fetched, AtlasError> {
    validate_url(url)?;
    if limits.max_response_bytes.is_some() {
        check_curl_capability()?;
    }
    // An explicit zero-second transfer budget is a budget that has
    // already run out — the same exactly-reproducible stop a
    // `job_deadline_secs` of 0 is. It is applied as written rather than
    // guessed away, and it cannot be expressed to `curl` (`--max-time 0`
    // is libcurl's "no timeout"), so it is refused here, before the
    // transfer it forbids.
    if limits.timeout_secs == Some(0) {
        return Err(AtlasError::InvalidRequest(format!(
            "this estate configured http_timeout_secs 0, a transfer budget that has already \
             expired, so {url} was not fetched"
        )));
    }
    let staging = staging_root.join(format!(".tmp-{}", Ulid::generate()));
    std::fs::create_dir(&staging)?;
    let mut guard = StagingGuard {
        dir: staging.clone(),
        kept: false,
    };
    let body_path = staging.join("body");
    let header_path = staging.join("headers");

    let started = std::time::Instant::now();
    let budget = limits.timeout_secs.map(std::time::Duration::from_secs);
    // Every URL this fetch has already requested, first one first. A
    // destination already in here is a cycle: the chain has come back to
    // something it served before, so following it cannot terminate.
    let mut requested: Vec<String> = vec![url.to_string()];
    let mut current = url.to_string();
    let mut hops: u32 = 0;

    let (final_url, final_status) = loop {
        let Ok(remaining) = remaining_budget(budget, started.elapsed()) else {
            return Err(AtlasError::SourceBytesUnavailable(format!(
                "fetching {url} exceeded the {}-second transfer budget this estate configured, \
                 after {hops} redirect(s)",
                budget.map(|budget| budget.as_secs()).unwrap_or_default()
            )));
        };
        let outcome = one_hop(
            verb,
            &current,
            limits,
            remaining,
            &header_path,
            &body_path,
            &staging,
            jobs,
            scope,
        )?;
        let Some(destination) = outcome.redirect else {
            if !(200..300).contains(&outcome.status) {
                return Err(AtlasError::SourceBytesUnavailable(format!(
                    "{url} answered HTTP {} (final URL {current})",
                    outcome.status
                )));
            }
            break (current, outcome.status);
        };
        hops += 1;
        if let Some(max) = limits.max_redirects
            && hops > max
        {
            return Err(AtlasError::InvalidRequest(format!(
                "fetching {url} followed more than the {max} redirect(s) this estate configured"
            )));
        }
        // The destination has to be admissible on its own terms, not
        // merely reachable: same protocols, and still no credential in
        // the locator.
        validate_url(&destination).map_err(|error| {
            AtlasError::InvalidRequest(format!(
                "{current} redirects to a URL this policy does not admit: {error}"
            ))
        })?;
        if requested.iter().any(|seen| seen == &destination) {
            return Err(AtlasError::InvalidRequest(format!(
                "fetching {url} is a redirect cycle: hop {hops} points back to {destination}, \
                 which this fetch has already requested, so following the chain cannot terminate"
            )));
        }
        requested.push(destination.clone());
        current = destination;
    };

    // `--max-filesize 0` cannot be handed to `curl` (it disables the
    // limit), and a bound `curl` did enforce is still re-checked here
    // against what actually landed: the refusal an operator configured
    // holds whichever side observes it.
    let staged_len = std::fs::metadata(&body_path)
        .map_err(|err| {
            AtlasError::SourceBytesUnavailable(format!(
                "{url} response body could not be measured: {err}"
            ))
        })?
        .len();
    if let Some(max_bytes) = limits.max_response_bytes
        && staged_len > max_bytes
    {
        return Err(AtlasError::InvalidRequest(format!(
            "response from {url} exceeds the {max_bytes}-byte bounded response size this estate \
             configured"
        )));
    }
    let header_text = std::fs::read_to_string(&header_path).unwrap_or_default();
    let headers = last_header_block(&header_text);
    let etag = header_value(&headers, "etag").map(str::to_owned);
    let last_modified = header_value(&headers, "last-modified").map(str::to_owned);
    let content_type = header_value(&headers, "content-type").map(str::to_owned);

    let digest = hash_file(&body_path).map_err(|err| {
        AtlasError::SourceBytesUnavailable(format!("{url} response body could not be read: {err}"))
    })?;
    let origin = HttpOrigin {
        requested_url: url.to_string(),
        final_url,
        status: final_status,
        etag,
        last_modified,
        content_type,
        // The moment this process observed the fetch complete — never
        // presented as an upstream publication or revision date; see
        // this module's own top-level doc and `HttpOrigin`'s.
        fetched_at_unix_millis: crate::domain::now_unix_millis(),
    };
    guard.kept = true;
    Ok(Fetched {
        revision: digest.clone(),
        content: format!("sha256:{digest}"),
        origin,
        staging,
        body: body_path,
    })
}

/// One request, under the same bounded-child containment every hop gets.
#[allow(clippy::too_many_arguments)]
fn one_hop(
    verb: &str,
    url: &str,
    limits: &FetchLimits,
    remaining: Option<TransferBudget>,
    header_path: &Path,
    body_path: &Path,
    staging: &Path,
    jobs: &crate::store::JobContext,
    scope: &str,
) -> Result<HopOutcome, AtlasError> {
    let mut command = Command::new("curl");
    command
        .env_clear()
        .args(fetch_args(url, limits, remaining, header_path, body_path))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // The staging directory travels with the job registration, so a
    // cancelled or reaped fetch has its partial body removed by the same
    // mechanism every other bounded child's staging is.
    let finished =
        match jobs
            .child(verb, scope, Some(staging.to_path_buf()))
            .run(command, |_stdin| {
                // No request body: this fetch is a GET. `BoundedChild::run`
                // closes the write end on return, which is stdin's whole
                // contribution here.
                Ok(())
            }) {
            wirk_core::jobs::ChildEnd::Finished(output) => output,
            wirk_core::jobs::ChildEnd::Cancelled { reason, .. } => {
                return Err(AtlasError::Cancelled(format!(
                    "http fetch of {url} {reason}"
                )));
            }
            wirk_core::jobs::ChildEnd::Failed(detail) => {
                return Err(AtlasError::SourceBytesUnavailable(format!(
                    "curl for {url} could not be started: {detail}"
                )));
            }
        };

    if !finished.status.success() {
        let code = finished.status.code();
        let stderr = String::from_utf8_lossy(&finished.stderr).trim().to_string();
        // 63 is curl's own documented exit code for "Maximum file size
        // exceeded" (`--max-filesize`): a bounded refusal, not a
        // transport failure, so it is reported as the request this
        // estate cannot run as asked rather than as the source being
        // unavailable.
        return if code == Some(63) {
            Err(AtlasError::InvalidRequest(format!(
                "response from {url} exceeds the {}-byte bounded response size this estate \
                 configured",
                limits.max_response_bytes.unwrap_or_default()
            )))
        } else {
            Err(AtlasError::SourceBytesUnavailable(format!(
                "curl for {url} exited {}: {stderr}",
                code.map(|c| c.to_string())
                    .unwrap_or_else(|| "by signal".into())
            )))
        };
    }

    let stdout = String::from_utf8_lossy(&finished.stdout);
    let mut fields = stdout.trim_end_matches(['\r', '\n']).splitn(3, '\t');
    let (Some(status_text), Some(_effective), redirect) =
        (fields.next(), fields.next(), fields.next())
    else {
        return Err(AtlasError::SourceBytesUnavailable(format!(
            "curl for {url} produced no parseable status/effective-url line"
        )));
    };
    let status: u16 = status_text.trim().parse().unwrap_or(0);
    let redirect = redirect
        .map(str::trim)
        .filter(|destination| !destination.is_empty())
        .map(str::to_owned);
    Ok(HopOutcome { status, redirect })
}

/// Attaches generation-dependent identity — retrieval unit ids, which
/// fold in the real `GenerationId` `capture` cannot know until its
/// revision/content are already computed from the fetched bytes — to
/// the one resource one fetch produced. Parallel to `doctree::finish`:
/// entirely from memory, no second read and no second fetch.
///
/// This extractor edition's own family rules decide, from the recorded
/// path and the fetched bytes together, exactly as they do for a Git blob
/// or a collected file: a response whose content is a document `anydoc`
/// reads is admitted as one even where neither the URL nor the origin's
/// declared type named a format, a text response is admitted unless it
/// carries a NUL byte no text extractor should be handed, and anything
/// neither names nor content can place is reported `Unsupported`.
pub(crate) fn finish(
    generation: &GenerationId,
    policy: &ExtractorPolicy,
    url: &str,
    digest: &str,
    bytes: &[u8],
    content_type: Option<&str>,
) -> ResourceRecord {
    let relative = resource_path(url, policy, content_type);
    let unsupported = |path: Vec<u8>, detail: &str| ResourceRecord {
        path,
        mode: "100644".into(),
        object_id: Some(digest.to_string()),
        byte_len: Some(bytes.len() as u64),
        disposition: CoverageDisposition::Unsupported,
        detail: Some(detail.to_string()),
        units: vec![],
    };
    match policy.family(&relative, bytes) {
        None => return unsupported(relative, "no extractor for path family"),
        // A document's own bytes are expected to be binary and are read
        // as the container they are, so the NUL screen must not preempt
        // them.
        Some(crate::ContentFamily::Document) => {}
        Some(_) if bytes.contains(&0) => {
            return unsupported(relative, "binary response body");
        }
        Some(_) => {}
    }
    match policy.units(generation, &relative, digest, bytes) {
        Ok(units) => ResourceRecord {
            path: relative,
            mode: "100644".into(),
            object_id: Some(digest.to_string()),
            byte_len: Some(bytes.len() as u64),
            disposition: CoverageDisposition::Indexed,
            detail: None,
            units,
        },
        Err(detail) => ResourceRecord {
            path: relative,
            mode: "100644".into(),
            object_id: Some(digest.to_string()),
            byte_len: Some(bytes.len() as u64),
            disposition: CoverageDisposition::Error,
            detail: Some(detail.to_string()),
            units: vec![],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_schemes_are_admitted() {
        assert!(validate_url("https://example.invalid/x").is_ok());
        assert!(validate_url("http://example.invalid/x").is_ok());
        assert!(validate_url("ftp://example.invalid/x").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("not-a-url").is_err());
    }

    #[test]
    fn a_credential_bearing_locator_is_refused() {
        let err = validate_url("https://user:pass@example.invalid/x").unwrap_err();
        assert!(matches!(err, AtlasError::InvalidRequest(_)));
    }

    #[test]
    fn a_path_traversal_segment_is_refused() {
        assert!(validate_url("https://example.invalid/a/../b").is_err());
    }

    #[test]
    fn resource_path_keeps_an_existing_recognised_extension() {
        let policy = ExtractorPolicy::default();
        let path = resource_path("https://example.invalid/readme.md", &policy, None);
        assert_eq!(path, b"readme.md");
    }

    #[test]
    fn resource_path_infers_html_from_content_type_when_the_url_has_no_extension() {
        let policy = ExtractorPolicy::default();
        let path = resource_path(
            "https://example.invalid/running-a-nats-service/introduction",
            &policy,
            Some("text/html; charset=utf-8"),
        );
        assert_eq!(path, b"running-a-nats-service/introduction.html");
    }

    #[test]
    fn resource_path_names_a_bare_host_index() {
        let policy = ExtractorPolicy::default();
        let path = resource_path("https://example.invalid/", &policy, Some("text/html"));
        assert_eq!(path, b"index.html");
    }

    #[test]
    fn last_header_block_reads_only_the_final_redirect_hop() {
        let text = "HTTP/1.1 301 Moved\r\nLocation: https://example.invalid/next\r\n\r\nHTTP/1.1 \
                     200 OK\r\nETag: \"abc\"\r\nContent-Type: text/html\r\n\r\n";
        let headers = last_header_block(text);
        assert_eq!(header_value(&headers, "etag"), Some("\"abc\""));
        assert_eq!(header_value(&headers, "location"), None);
    }

    #[test]
    fn the_config_suppression_flag_is_first_and_globbing_is_off() {
        let limits = FetchLimits {
            max_response_bytes: Some(8),
            timeout_secs: Some(9),
            max_redirects: Some(3),
        };
        let args = fetch_args(
            "http://example.invalid/{a,b}",
            &limits,
            TransferBudget::from_remaining(std::time::Duration::from_secs(9)),
            Path::new("/tmp/h"),
            Path::new("/tmp/b"),
        );
        // Positional: `-q` only suppresses curl's config file as the
        // first argument, so this is the contract, not a preference.
        assert_eq!(args[0], "-q");
        assert!(args.iter().any(|arg| arg == "--globoff"));
        // The literal URL travels last, after `--`, exactly as admitted.
        assert_eq!(args[args.len() - 2], "--");
        assert_eq!(args[args.len() - 1], "http://example.invalid/{a,b}");
        // Every bound comes from the policy, never a constant here.
        assert!(args.iter().any(|arg| arg == "8"));
        assert!(args.iter().any(|arg| arg == "9.000"));
        // The hop count is this policy's own to apply across the chain:
        // `curl` is never asked to follow one, so it is never told how
        // many to follow either.
        assert!(!args.iter().any(|arg| arg == "-L"));
        assert!(!args.iter().any(|arg| arg == "--max-redirs"));
        // What is about authority rather than about counting stays on
        // every hop.
        let proto = args
            .iter()
            .position(|arg| arg == "--proto")
            .expect("every hop is confined to http/https");
        assert_eq!(args[proto + 1], "=http,https");
    }

    /// The representability boundary, pinned at the value it turns on.
    ///
    /// `curl --max-time` renders as a decimal but libcurl's control is
    /// `CURLOPT_TIMEOUT_MS`, whose documentation states that zero means
    /// no transfer timeout. So a remainder under one millisecond has no
    /// positive form here, and the two wrong answers are *both* excluded
    /// by construction: it is never rendered as the disabling `0.000`,
    /// and it is never rounded up into time the operator did not write.
    ///
    /// Watched failing against `967870c`, where `fetch_args` formatted
    /// `remaining.as_secs_f64()` with `{:.3}` and
    /// `Duration::from_micros(400)` came out as `--max-time 0.000`.
    #[test]
    fn a_remainder_too_small_to_express_is_spent_not_rendered_as_no_timeout() {
        use std::time::Duration;
        // Under a millisecond: nothing positive is representable.
        assert_eq!(
            TransferBudget::from_remaining(Duration::from_micros(400)),
            None
        );
        assert_eq!(
            TransferBudget::from_remaining(Duration::from_micros(999)),
            None
        );
        assert_eq!(TransferBudget::from_remaining(Duration::ZERO), None);
        // Exactly one millisecond is the smallest thing that is.
        let smallest = TransferBudget::from_remaining(Duration::from_micros(1000))
            .expect("1ms is expressible");
        assert_eq!(smallest.max_time_arg(), "0.001");
        // Truncation, never extension: 1.9ms is one millisecond of
        // budget, not two.
        let truncated = TransferBudget::from_remaining(Duration::from_micros(1900))
            .expect("1.9ms is expressible");
        assert_eq!(truncated.max_time_arg(), "0.001");
        assert_eq!(
            TransferBudget::from_remaining(Duration::from_millis(1_500))
                .expect("1.5s")
                .max_time_arg(),
            "1.500"
        );
        // And nothing `fetch_args` can be handed renders the disabling
        // value, because the only thing it accepts is one of these.
        for micros in [1_000u64, 1_900, 2_000, 999_999, 1_000_000, 61_000_000] {
            let budget =
                TransferBudget::from_remaining(Duration::from_micros(micros)).expect("expressible");
            let args = fetch_args(
                "http://example.invalid/x",
                &FetchLimits::default(),
                Some(budget),
                Path::new("/tmp/h"),
                Path::new("/tmp/b"),
            );
            let at = args
                .iter()
                .position(|arg| arg == "--max-time")
                .expect("a budget is passed");
            assert_ne!(
                args[at + 1],
                "0.000",
                "curl reads 0 as no timeout; {micros}us must never render it"
            );
        }
    }

    /// The whole-chain accounting rule, checked as the arithmetic it is
    /// rather than against a clock: each hop receives what is left of the
    /// one budget the operator wrote, and a chain that has spent it is
    /// refused rather than given a fresh copy.
    #[test]
    fn each_hop_receives_the_remainder_of_one_budget_not_a_fresh_copy() {
        use std::time::Duration;
        let budget = Some(Duration::from_secs(2));
        // Nothing configured stays nothing configured.
        assert_eq!(remaining_budget(None, Duration::from_secs(9)), Ok(None));
        // A hop that has spent half of it gets the other half, not two
        // seconds again.
        assert_eq!(
            remaining_budget(budget, Duration::from_millis(500))
                .expect("still running")
                .expect("still expressible")
                .max_time_arg(),
            "1.500"
        );
        // Spent exactly, spent over, and spent to within less than a
        // millisecond all read the same way: there is no hop left.
        assert_eq!(remaining_budget(budget, Duration::from_secs(2)), Err(()));
        assert_eq!(remaining_budget(budget, Duration::from_secs(3)), Err(()));
        assert_eq!(
            remaining_budget(budget, Duration::from_micros(1_999_600)),
            Err(()),
            "a 400us remainder is spent, not a disabled timeout"
        );
    }

    /// `%{redirect_url}` is what makes a cycle nameable, so it is part
    /// of the write-out contract, not an incidental extra field.
    #[test]
    fn every_hop_asks_curl_where_a_redirect_would_have_gone() {
        let args = fetch_args(
            "http://example.invalid/x",
            &FetchLimits::default(),
            None,
            Path::new("/tmp/h"),
            Path::new("/tmp/b"),
        );
        let write_out = args
            .iter()
            .position(|arg| arg == "-w")
            .expect("a write-out format is always requested");
        assert_eq!(
            args[write_out + 1],
            "%{http_code}\t%{url_effective}\t%{redirect_url}"
        );
    }

    /// With nothing configured, no bound is *invented* to pass to
    /// `curl`. A size and a clock option carrying a number nobody chose
    /// would be this product deciding how large a resource may be and
    /// how long a transfer may take, which is the thing ruling 0401
    /// removes.
    ///
    /// Watched failing against the previous defaults, where these came
    /// out as `--max-filesize 8388608` and `--max-time 20`.
    #[test]
    fn an_unbounded_fetch_passes_no_size_or_time_option_at_all() {
        let limits = FetchLimits::default();
        let args = fetch_args(
            "http://example.invalid/x",
            &limits,
            None,
            Path::new("/tmp/h"),
            Path::new("/tmp/b"),
        );
        assert!(
            !args.iter().any(|arg| arg == "--max-filesize"),
            "no size option is passed when no size bound was asked for: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "--max-time"),
            "no clock option is passed when no clock was asked for: {args:?}"
        );
    }

    /// `--max-filesize 0` is documented to *disable* `curl`'s limit, so
    /// an explicit `http_max_response_bytes` of 0 — "admit no body
    /// larger than nothing" — must not be handed to it. It is applied by
    /// this policy against the staged body instead, which is the only
    /// place the written value keeps its meaning.
    #[test]
    fn a_zero_response_bound_is_never_handed_to_curls_own_disabling_value() {
        let limits = FetchLimits {
            max_response_bytes: Some(0),
            timeout_secs: None,
            max_redirects: None,
        };
        let args = fetch_args(
            "http://example.invalid/x",
            &limits,
            None,
            Path::new("/tmp/h"),
            Path::new("/tmp/b"),
        );
        assert!(
            !args.iter().any(|arg| arg == "--max-filesize"),
            "0 would disable curl's own limit, which is the opposite of what was written: {args:?}"
        );
    }

    #[test]
    fn the_streamed_size_bound_is_claimed_only_where_curl_can_enforce_it() {
        assert_eq!(
            parse_curl_version("curl 8.18.0 (x86_64) libcurl/8.18.0"),
            Some((8, 18, 0))
        );
        assert_eq!(
            parse_curl_version("curl 7.81.0 (x86_64-pc-linux-gnu)"),
            Some((7, 81, 0))
        );
        assert_eq!(
            parse_curl_version("curl 8.4.0-DEV (x86_64)"),
            Some((8, 4, 0))
        );
        assert_eq!(parse_curl_version("not curl at all"), None);
        assert_eq!(parse_curl_version(""), None);

        // `--max-filesize` aborts an undeclared-length transfer only from
        // 8.4.0; below it, this estate's own bound is not something this
        // policy can honestly say it applies.
        assert!(enforces_unknown_length_bound((8, 18, 0)));
        assert!(enforces_unknown_length_bound((8, 4, 0)));
        assert!(!enforces_unknown_length_bound((8, 3, 9)));
        assert!(!enforces_unknown_length_bound((7, 81, 0)));
    }

    #[test]
    fn a_document_response_is_admitted_by_its_content_not_only_its_url() {
        let policy = ExtractorPolicy::default();
        let generation = GenerationId("g-test".into());
        let rtf = b"{\\rtf1\\ansi\\deff0 {\\fonttbl{\\f0 Times;}}\\f0 Httpmarker prose.\\par}";
        let record = finish(
            &generation,
            &policy,
            "https://example.invalid/briefing",
            &hash_hex(rtf),
            rtf,
            Some("application/octet-stream"),
        );
        assert_eq!(record.path, b"briefing");
        assert_eq!(
            record.disposition,
            CoverageDisposition::Indexed,
            "{:?}",
            record.detail
        );
        assert_eq!(
            record.units.first().map(|unit| unit.family),
            Some(crate::ContentFamily::Document)
        );
    }

    #[test]
    fn a_response_neither_name_nor_content_can_place_stays_unsupported() {
        let policy = ExtractorPolicy::default();
        let generation = GenerationId("g-test".into());
        let record = finish(
            &generation,
            &policy,
            "https://example.invalid/blob",
            &hash_hex(b"\x00\x01binary"),
            b"\x00\x01binary",
            Some("application/octet-stream"),
        );
        assert_eq!(record.disposition, CoverageDisposition::Unsupported);
        assert_eq!(
            record.detail.as_deref(),
            Some("no extractor for path family")
        );
    }
}
