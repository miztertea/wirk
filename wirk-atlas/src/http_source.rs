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
use std::path::Path;
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
#[derive(Debug, Clone, Copy)]
pub(crate) struct FetchLimits {
    pub(crate) max_response_bytes: u64,
    pub(crate) timeout_secs: u64,
    pub(crate) max_redirects: u32,
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

/// What this host's own `curl` is, asked once per fetch, before any
/// network access.
///
/// The bound an operator configured has to be one this policy can
/// actually apply, and on an older `curl` it is not: a chunked body would
/// stream past `http_max_response_bytes` and only be refused after its
/// bytes were already staged. Rather than advertise an enforcement that
/// is not there, this refuses the acquisition with the version it found,
/// the version it needs, and what the operator can do about it. `-q` is
/// first here for the same reason it is first in the fetch itself.
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

/// Exactly the argument vector one fetch hands `curl`, in order, so the
/// two controls whose correctness is positional can be checked without a
/// network.
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
fn fetch_args(
    url: &str,
    limits: &FetchLimits,
    header_path: &Path,
    body_path: &Path,
) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |value: &str| args.push(OsString::from(value));
    push("-q");
    push("--globoff");
    push("-sS");
    push("-L");
    push("--max-redirs");
    push(&limits.max_redirects.to_string());
    push("--proto");
    push("=http,https");
    push("--proto-redir");
    push("=http,https");
    push("--max-time");
    push(&limits.timeout_secs.to_string());
    push("--max-filesize");
    push(&limits.max_response_bytes.to_string());
    push("-D");
    args.push(header_path.into());
    args.push(OsString::from("-o"));
    args.push(body_path.into());
    args.push(OsString::from("-w"));
    // One transfer, so one line: `--globoff` above is what makes that
    // true, and `%{url_effective}` is then the final URL of the one
    // response this fetch actually read.
    args.push(OsString::from("%{http_code} %{url_effective}"));
    args.push(OsString::from("--"));
    args.push(OsString::from(url));
    args
}

/// One bounded fetch of `url`, run through this host's own installed
/// `curl` under `jobs`'s existing bounded-child containment (P4.5 B2):
/// process group, per-job cgroup where available, a wall-clock deadline
/// and a registered cancel token an operator's `atlas cancel --source`
/// reaches, exactly as `semantic.rs`'s backend invocation already gets.
/// `curl` itself refuses a transfer that exceeds `limits.max_response_bytes`
/// (`--max-filesize`, exit 63) and one that exceeds
/// `limits.timeout_secs` (`--max-time`) or `limits.max_redirects`
/// (`--max-redirs`) — bounded IO before this crate reads a single byte
/// back, not a cap applied after an unbounded read.
///
/// Returns the manifest identity (bare 64-hex SHA-256 `revision`, the
/// same value `"sha256:"`-tagged as `content` — this policy's whole
/// replacement for `git commit`/`tree`, parallel to `doctree`'s), the
/// disclosed [`HttpOrigin`], and the raw response bytes — for the
/// caller to fold into a `GenerationId` (`finish`, below, needs it) and
/// to persist through `AtlasStore::stage`.
pub(crate) fn capture(
    verb: &str,
    url: &str,
    limits: &FetchLimits,
    staging_root: &Path,
    jobs: &crate::store::JobContext,
    scope: &str,
) -> Result<(String, String, HttpOrigin, Vec<u8>), AtlasError> {
    validate_url(url)?;
    check_curl_capability()?;
    let staging = staging_root.join(format!(".tmp-{}", Ulid::generate()));
    std::fs::create_dir(&staging)?;
    let body_path = staging.join("body");
    let header_path = staging.join("headers");

    let mut command = Command::new("curl");
    command
        .env_clear()
        .args(fetch_args(url, limits, &header_path, &body_path))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let cleanup = |staging: &Path| {
        let _ = std::fs::remove_dir_all(staging);
    };

    let finished = match jobs
        .child(verb, scope, Some(staging.clone()))
        .run(command, |_stdin| {
            // No request body: this fetch is a GET. `BoundedChild::run`
            // closes the write end on return, which is stdin's whole
            // contribution here.
            Ok(())
        }) {
        wirk_core::jobs::ChildEnd::Finished(output) => output,
        wirk_core::jobs::ChildEnd::Cancelled { reason, .. } => {
            cleanup(&staging);
            return Err(AtlasError::Cancelled(format!(
                "http fetch of {url} {reason}"
            )));
        }
        wirk_core::jobs::ChildEnd::Failed(detail) => {
            cleanup(&staging);
            return Err(AtlasError::SourceBytesUnavailable(format!(
                "curl for {url} could not be started: {detail}"
            )));
        }
    };

    if !finished.status.success() {
        let code = finished.status.code();
        let stderr = String::from_utf8_lossy(&finished.stderr).trim().to_string();
        cleanup(&staging);
        // 63 is curl's own documented exit code for "Maximum file size
        // exceeded" (`--max-filesize`): a bounded refusal, not a
        // transport failure, so it is reported as the request this
        // estate cannot run as asked rather than as the source being
        // unavailable.
        return if code == Some(63) {
            Err(AtlasError::InvalidRequest(format!(
                "response from {url} exceeds the {}-byte bounded response size",
                limits.max_response_bytes
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
    let Some((status_text, final_url)) = stdout.trim().split_once(' ') else {
        cleanup(&staging);
        return Err(AtlasError::SourceBytesUnavailable(format!(
            "curl for {url} produced no parseable status/effective-url line"
        )));
    };
    let status: u16 = status_text.parse().unwrap_or(0);
    if !(200..300).contains(&status) {
        cleanup(&staging);
        return Err(AtlasError::SourceBytesUnavailable(format!(
            "{url} answered HTTP {status_text} (final URL {final_url})"
        )));
    }

    let bytes = match std::fs::read(&body_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            cleanup(&staging);
            return Err(AtlasError::SourceBytesUnavailable(format!(
                "{url} response body could not be read back: {err}"
            )));
        }
    };
    if bytes.len() as u64 > limits.max_response_bytes {
        cleanup(&staging);
        return Err(AtlasError::InvalidRequest(format!(
            "response from {url} exceeds the {}-byte bounded response size",
            limits.max_response_bytes
        )));
    }
    let header_text = std::fs::read_to_string(&header_path).unwrap_or_default();
    let headers = last_header_block(&header_text);
    let etag = header_value(&headers, "etag").map(str::to_owned);
    let last_modified = header_value(&headers, "last-modified").map(str::to_owned);
    let content_type = header_value(&headers, "content-type").map(str::to_owned);
    cleanup(&staging);

    let digest = hash_hex(&bytes);
    let origin = HttpOrigin {
        requested_url: url.to_string(),
        final_url: final_url.to_string(),
        status,
        etag,
        last_modified,
        content_type,
        // The moment this process observed the fetch complete — never
        // presented as an upstream publication or revision date; see
        // this module's own top-level doc and `HttpOrigin`'s.
        fetched_at_unix_millis: crate::domain::now_unix_millis(),
    };

    Ok((digest.clone(), format!("sha256:{digest}"), origin, bytes))
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
            max_response_bytes: 8,
            timeout_secs: 9,
            max_redirects: 3,
        };
        let args = fetch_args(
            "http://example.invalid/{a,b}",
            &limits,
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
        assert!(args.iter().any(|arg| arg == "9"));
        assert!(args.iter().any(|arg| arg == "3"));
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
