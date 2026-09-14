//! `wirk atlas ...` (P3 W3, `source-orient/BUILD-BRIEF.md` "Public
//! surface"): seven thin JSON-capable clients over wirkd's own Atlas
//! verbs (`crate::wirkd::{AtlasAcquirePayload, ...}`). This module
//! parses argv and prints; `wirkd::server::handle_atlas_*` owns every
//! actual admission/store decision — commands send argv as data, never
//! shell text, and `--json` is the acceptance surface (BUILD-BRIEF.md);
//! the default rendering below is a minimal, practical text summary,
//! never the schema of record.

use std::path::Path;
use std::process::ExitCode;

use crate::wirkd::{
    AtlasAcquirePayload, AtlasCancelPayload, AtlasDocumentPayload, AtlasFindingsPayload,
    AtlasPublishPayload, AtlasRefreshPayload, AtlasRelatePayload, AtlasRemovePayload,
    AtlasResolvePayload, AtlasSearchPayload, AtlasSemanticBuildPayload, AtlasSemanticSelectPayload,
    AtlasStatusPayload, CancelTarget, Reply, Request,
};
use crate::{ActorContext, actor_context, flag_value, warn_if_index_incomplete, wirkd_client_call};
use wirk_core::WorkId;

pub fn atlas_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("acquire") => acquire_command(&rest[1..]),
        Some("refresh") => refresh_command(&rest[1..]),
        Some("publish") => publish_command(&rest[1..]),
        Some("remove") => remove_command(&rest[1..]),
        Some("status") => status_command(&rest[1..]),
        // P4.5 B2 correction: the operator's reachable cancellation
        // path. Before it, `cancel_jobs` had no caller and nothing but
        // a deadline could stop a running backend child.
        Some("cancel") => cancel_command(&rest[1..]),
        Some("search") => search_command(&rest[1..]),
        Some("resolve") => resolve_command(&rest[1..]),
        // P5: the structured document reader beside the byte resolver —
        // what a source actually holds, and its embedded assets on
        // demand.
        Some("document") => document_command(&rest[1..]),
        Some("relate") => relate_command(&rest[1..]),
        Some("semantic") => semantic_command(&rest[1..]),
        // W-B (§7): the estate's derived, rebuildable Findings index —
        // alongside W3's own seven verbs and W4-A's semantic pair, not
        // one of them.
        Some("findings") => findings_command(&rest[1..]),
        _ => atlas_usage(),
    }
}

fn atlas_usage() -> ExitCode {
    eprintln!(
        "usage: wirk atlas acquire --estate <root> --source <name> --repository <path-or-url> --revision <ref> [--kind git|document-tree|http] [--requesting-work <id> | --admin] [--json] \
         (--repository names a Git repository/subdirectory/worktree under --kind git, the default; \
          a plain local directory under --kind document-tree; one explicit public http:// or \
          https:// URL under --kind http, with no embedded credential and no \"..\" path segment. \
          --revision names the Git ref to acquire under --kind git, required; under --kind \
          document-tree or --kind http it can only mean the source's own last-observed current \
          state, so it is optional there, defaults to \"current\", and any other value is refused \
          by name — neither policy has another revision to honour) \
         | wirk atlas refresh --estate <root> --source <name> [--revision <ref>] [--requesting-work <id> | --admin] [--json] \
         (--revision is optional: omitted, the source's own registered acquisition policy \
          supplies it — the ref a Git source was admitted to track, or a document tree's \
          current state; --requesting-work/--admin on any of these three resolve the Work the \
          job belongs to the same way `atlas semantic build` does — the actor's own Work inside \
          a context, or an explicit administrative job — so `atlas cancel` scoped to that Work \
          can reach it — a document collection's walk, its extraction and a publish's \
          revalidation all run as registered, cancellable jobs) \
         | wirk atlas publish --estate <root> --source <name> --generation <id> [--requesting-work <id> | --admin] [--json] \
         | wirk atlas remove --estate <root> --source <name> [--json] \
         | wirk atlas status --estate <root> [--source <name>] [--work <id>] [--json] \
         | wirk atlas cancel --estate <root> (--list | --job <id> | --source <name> | --all) [--reason <text>] [--wait <secs>] [--requesting-work <id> | --admin] [--json] \
         | wirk atlas resolve [--estate <root>] [--work <id> | --admin] --coordinate <encoded> [--json] \
         | wirk atlas document [--estate <root>] [--work <id> | --admin] --coordinate <encoded> [--asset <id> --output <path>] [--json] \
         (reads the source a coordinate names through the native document reader: without \
          --asset, its structure and an inventory of the assets it embeds, with no asset bytes; \
          with --asset <id> from that inventory, that one asset's bytes, written to --output \
          <path>, which is required for them — binary payloads are never printed) \
         | wirk atlas search [--estate <root>] [--work <id> | --admin] --query <text> [--source <name>] [--semantic requested|disabled] [--semantic-backend <path>] [--semantic-backend-arg <arg>...] [--semantic-model <dir>] [--family code|knowledge|config|document]... [--limit <n>] [--capacity <n>] [--continue <token>] [--json] \
         | wirk atlas semantic build --estate <root> --source <name> --generation <id> --backend <path> [--backend-arg <arg>...] --model <dir> [--chunker units|native] [--json] \
         | wirk atlas semantic select --estate <root> --source <name> --edition <id> [--json] \
         | wirk atlas relate --estate <root> --work <id> --kind governed_by --from <coordinate> --to <coordinate> --evidence <coordinate> [--evidence <coordinate>...] [--run <id>] [--world <hash>] [--json] \
         | wirk atlas findings --estate <root> (--requesting-work <id> | --admin [--rebuild | --retire-preserved-index]) [--json] \
         \n(search, resolve and document read under one scope rule: outside an actor context an \
          omitted --work is the operator's read of the whole estate, unchanged; inside one, \
          naming that actor's own estate reads as its own Work and says so, naming a different \
          estate with no explicit scope is refused, and --admin is the explicit administrative \
          read. Scope is decided daemon-side either way.)"
    );
    ExitCode::from(1)
}

fn is_json(rest: &[String]) -> bool {
    rest.iter().any(|arg| arg == "--json")
}

/// Every flag `wirk atlas <verb>` accepts, and whether it takes a value.
/// `--json` is the only boolean.
///
/// P3 W3 extractor completion (`W3-EXTRACTOR-COMPLETION.md`): before
/// this, argv was only ever *searched* for the flags a verb cared about
/// (`flag_value`/`flag_values` scan for a name and take the next
/// element), so anything else was silently ignored. Both native actors
/// in the previous stage passed `--source wirk` to `wirk atlas relate`,
/// which has no such flag; it was dropped without a word, and their
/// reports read as though it had been honoured. A misspelled or
/// unsupported flag must fail visibly instead of quietly changing what
/// the caller thinks the request was — the same honesty rule the JSON
/// dispositions already follow.
/// This module's own spelling of the shared checker: every verb here is
/// `wirk atlas <verb>`, so the prefix is supplied once instead of at
/// eleven call sites (R2 — one implementation, in `crate`).
fn check_flags(verb: &str, rest: &[String], allowed: &[(&str, bool)]) -> Result<(), ExitCode> {
    crate::check_flags(&format!("atlas {verb}"), rest, allowed)
}

/// The estate and Work one Atlas **reading** verb asks under.
///
/// Reading verbs used to select `(--estate, --work)` directly, which made
/// an explicit `--estate` discard the Work an actor was executing as: a
/// native caller inside one Work that named its own estate and omitted
/// `--work` was answered estate-wide. Acquisition and every other scoped
/// verb already resolved this through `crate::resolve_scope`; these now do
/// too, so one rule decides for all of them:
///
/// - Outside an actor context nothing changes: the operator's omitted
///   scope is still the administrative read of the whole estate.
/// - Inside one, naming the same estate preserves that actor's Work, and
///   says so on stderr rather than silently narrowing or widening.
/// - Inside one, naming a *different* estate with no explicit scope is
///   refused: this actor's Work is not a scope for another estate.
/// - `--admin` is the explicit, disclosed operator read, and `--work`
///   naming this actor's own Work stays legal and explicit.
/// - A half-injected context names no identity and is refused rather than
///   read as an operator shell.
///
/// The estate itself still comes from `--estate` when it is given and from
/// the injected context otherwise, so the `fetch` line a stage projection
/// prints runs verbatim inside a pane.
fn reading_scope(verb: &str, rest: &[String]) -> Result<(String, Option<WorkId>), ExitCode> {
    let estate = match flag_value(rest, "--estate") {
        Some(estate) => estate,
        None => match actor_context() {
            ActorContext::Present { estate_root, .. } => estate_root,
            ActorContext::Partial { missing } => {
                eprintln!(
                    "{verb}: --estate was not given and the injected context is incomplete ({}); \
                     name --estate <root> explicitly, or run inside a complete execution context",
                    missing.join(", ")
                );
                return Err(ExitCode::from(1));
            }
            ActorContext::Absent => return Err(atlas_usage()),
        },
    };
    let scope = crate::resolve_scope(
        verb,
        &estate,
        flag_value(rest, "--work"),
        rest.iter().any(|arg| arg == "--admin"),
    )
    .map_err(|refusal| {
        eprintln!("{verb}: {refusal}");
        ExitCode::from(1)
    })?;
    if let Some(note) = &scope.note {
        eprintln!("{verb}: {note}");
    }
    Ok((estate, scope.requesting))
}

const ESTATE: (&str, bool) = ("--estate", true);
const JSON: (&str, bool) = ("--json", false);

use crate::flag_values;

/// The first 16 characters of a digest, for a line a human reads. The
/// full value is always in `--json`; this never replaces it.
fn short(digest: &str) -> &str {
    digest.get(..16).unwrap_or(digest)
}

fn print_result(json: bool, result: &serde_json::Value, summary: impl FnOnce(&serde_json::Value)) {
    if json {
        println!("{result}");
    } else {
        summary(result);
    }
}

/// Like `wirkd_client_call`, but for a verb whose reply carries its own
/// `outcome` field distinguishing a real failure from a structurally
/// successful reply (P3 W3 correction, ruling 0093, W3-CORRECTION.md
/// item 4; VERDICT.md L5: "failed operations return exit 0" — a failed
/// `refresh`/unresolvable `resolve` both did). `ok_outcomes` names which
/// `outcome` values count as success; anything else still prints the
/// full JSON (the diagnostic is never withheld) but exits non-zero.
fn call_expecting_outcome(
    estate: &str,
    request: &Request,
    ok_outcomes: &[&str],
    on_result: impl FnOnce(&serde_json::Value),
) -> ExitCode {
    let pointer = match crate::wirkd::client::locate(Path::new(estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk wirkd: {err}");
            return ExitCode::from(2);
        }
    };
    match crate::wirkd::client::call(&pointer.socket, request) {
        Ok(Reply::Ok { result, .. }) => {
            on_result(&result);
            let outcome = result["outcome"].as_str().unwrap_or("");
            if ok_outcomes.contains(&outcome) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Ok(Reply::Err { error, .. }) => {
            crate::render_refusal(&error);
            ExitCode::from(2)
        }
        Err(err) => {
            eprintln!("wirk wirkd: {err}");
            ExitCode::from(2)
        }
    }
}

fn acquire_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "acquire",
        rest,
        &[
            ESTATE,
            JSON,
            ("--source", true),
            ("--repository", true),
            ("--revision", true),
            ("--kind", true),
            ("--requesting-work", true),
            ("--admin", false),
        ],
    ) {
        return code;
    }
    let (Some(estate), Some(source), Some(repository)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--source"),
        flag_value(rest, "--repository"),
    ) else {
        return atlas_usage();
    };
    // Same resolution as every other scoped job-starting verb
    // (`semantic_build_command`): the actor's own Work inside a context,
    // `--admin` for an administrative job, and the operator's shell
    // unchanged. Without this a job registers with no requester and its
    // own actor cannot reach it through `atlas cancel`.
    let scope = match crate::resolve_scope(
        "wirk atlas acquire",
        &estate,
        flag_value(rest, "--requesting-work"),
        rest.iter().any(|arg| arg == "--admin"),
    ) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk atlas acquire: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk atlas acquire: {note}");
    }
    // The explicit, one-time choice of acquisition policy for a source
    // registered for the first time — `git`
    // (unchanged default) or `document-tree` for a local non-Git
    // document collection. Never inferred from `--repository`'s own
    // shape.
    let kind = flag_value(rest, "--kind");
    // A document-tree source has no revision beside
    // its own current state, so `--revision` is optional under `--kind
    // document-tree` and defaults to the sentinel the daemon actually
    // checks for (`wirk_atlas::DOCUMENT_TREE_CURRENT_OBSERVATION`) —
    // never silently defaulted to a made-up Git-shaped value. Any other
    // value the caller does supply travels unchanged and is refused, by
    // name, on the daemon side (`AtlasStore::register_document_tree`/
    // `acquire_document_tree`), which is the one place that can express
    // what this policy actually observed. Git's `--revision` stays
    // required exactly as before.
    let revision = match (flag_value(rest, "--revision"), kind.as_deref()) {
        (Some(value), _) => value,
        (None, Some("document-tree")) => wirk_atlas::DOCUMENT_TREE_CURRENT_OBSERVATION.to_string(),
        (None, Some("http")) => wirk_atlas::HTTP_SOURCE_CURRENT_OBSERVATION.to_string(),
        (None, _) => return atlas_usage(),
    };
    let json = is_json(rest);
    call_expecting_outcome(
        &estate,
        &Request::atlas_acquire(AtlasAcquirePayload {
            source,
            repository,
            revision,
            kind,
            // Who this job belongs to, so `atlas cancel --source` run
            // by the same Work can reach it. `None` is an administrative
            // job.
            work: scope.requesting,
        }),
        &["staged"],
        |result| {
            print_result(json, result, |result| {
                println!(
                    "outcome {} generation {} acquisition_policy {}",
                    result["outcome"].as_str().unwrap_or("?"),
                    result["generation"]["generation"].as_str().unwrap_or("-"),
                    result["membership"]["acquisition_policy"]
                        .as_str()
                        .unwrap_or("?")
                );
            });
        },
    )
}

/// `wirk atlas remove`: unregisters a source's own catalog membership. Refused, by name,
/// while this estate's own records still need this source's published
/// generation or selected semantic edition (a non-terminal Work's
/// delivered World, or an unsettled finding) — see
/// `wirk_atlas::AtlasStore::remove_source`'s own doc for why that check
/// happens here, not inside `wirk-atlas`. Never removes the source's
/// own original files. Once removed, this source's own generation/
/// edition bytes on disk are unreferenced but not yet reclaimed: `wirk
/// estate clean --class atlas-generations|atlas-editions
/// --all-unreferenced` does that, re-deriving retention against every
/// other source first.
fn remove_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags("remove", rest, &[ESTATE, JSON, ("--source", true)]) {
        return code;
    }
    let (Some(estate), Some(source)) = (flag_value(rest, "--estate"), flag_value(rest, "--source"))
    else {
        return atlas_usage();
    };
    let json = is_json(rest);
    call_expecting_outcome(
        &estate,
        &Request::atlas_remove(AtlasRemovePayload { source }),
        &["removed"],
        |result| {
            print_result(json, result, |result| {
                println!(
                    "outcome {} released_generation {} released_edition {}",
                    result["outcome"].as_str().unwrap_or("?"),
                    result["released_generation"].as_str().unwrap_or("-"),
                    result["released_edition"].as_str().unwrap_or("-"),
                );
            });
        },
    )
}

fn refresh_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "refresh",
        rest,
        &[
            ESTATE,
            JSON,
            ("--source", true),
            ("--revision", true),
            ("--requesting-work", true),
            ("--admin", false),
        ],
    ) {
        return code;
    }
    // `--revision` is optional: a source registered as a document
    // collection has exactly one observable state, and a Git source
    // already records the ref it was admitted to track. Omitted, the
    // daemon applies the membership's own policy default rather than
    // making the caller spell one.
    let (Some(estate), Some(source)) = (flag_value(rest, "--estate"), flag_value(rest, "--source"))
    else {
        return atlas_usage();
    };
    // Same resolution as every other scoped job-starting verb
    // (`semantic_build_command`): the actor's own Work inside a context,
    // `--admin` for an administrative job, and the operator's shell
    // unchanged. Without this a job registers with no requester and its
    // own actor cannot reach it through `atlas cancel`.
    let scope = match crate::resolve_scope(
        "wirk atlas refresh",
        &estate,
        flag_value(rest, "--requesting-work"),
        rest.iter().any(|arg| arg == "--admin"),
    ) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk atlas refresh: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk atlas refresh: {note}");
    }
    let revision = flag_value(rest, "--revision");
    let json = is_json(rest);
    call_expecting_outcome(
        &estate,
        &Request::atlas_refresh(AtlasRefreshPayload {
            source,
            revision,
            work: scope.requesting,
        }),
        &["staged"],
        |result| {
            print_result(json, result, |result| {
                println!(
                    "outcome {} generation {}",
                    result["outcome"].as_str().unwrap_or("?"),
                    result["generation"]["generation"].as_str().unwrap_or("-")
                );
            });
        },
    )
}

fn publish_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "publish",
        rest,
        &[
            ESTATE,
            JSON,
            ("--source", true),
            ("--generation", true),
            ("--requesting-work", true),
            ("--admin", false),
        ],
    ) {
        return code;
    }
    let (Some(estate), Some(source), Some(generation)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--source"),
        flag_value(rest, "--generation"),
    ) else {
        return atlas_usage();
    };
    // Same resolution as every other scoped job-starting verb
    // (`semantic_build_command`): the actor's own Work inside a context,
    // `--admin` for an administrative job, and the operator's shell
    // unchanged. Without this a document publish's revalidation job
    // registers with no requester and its own actor cannot reach it
    // through `atlas cancel`.
    let scope = match crate::resolve_scope(
        "wirk atlas publish",
        &estate,
        flag_value(rest, "--requesting-work"),
        rest.iter().any(|arg| arg == "--admin"),
    ) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk atlas publish: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk atlas publish: {note}");
    }
    let json = is_json(rest);
    wirkd_client_call(
        &estate,
        &Request::atlas_publish(AtlasPublishPayload {
            source,
            generation,
            work: scope.requesting,
        }),
        |result| {
            print_result(json, result, |result| {
                println!(
                    "published {} at revision {}",
                    result["generation"].as_str().unwrap_or("?"),
                    result["publication_revision"].as_u64().unwrap_or(0)
                );
            });
        },
    )
}

/// `wirk atlas cancel`: stop a running expensive Atlas job.
///
/// The target is required and explicit — `--job`, `--source` or `--all`
/// — because a cancellation that reaches further than the operator meant
/// is the failure worth designing against. `--list` names no target and
/// cancels nothing; it reports what is running, which is also how an
/// operator observes whether a job they cancelled has actually ended.
///
/// Signalling and stopping are reported separately. Without `--wait` the
/// reply says only that the jobs were signalled, because at that instant
/// that is all that is known.
fn cancel_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "cancel",
        rest,
        &[
            ESTATE,
            JSON,
            ("--list", false),
            ("--all", false),
            ("--job", true),
            ("--source", true),
            ("--reason", true),
            ("--wait", true),
            ("--requesting-work", true),
            ("--admin", false),
        ],
    ) {
        return code;
    }
    let Some(estate) = flag_value(rest, "--estate") else {
        return atlas_usage();
    };
    // Ruling 0251 F4, through ruling 0117's existing resolution. Inside
    // an actor context this verb is asked as that actor's own Work, so
    // it reaches that Work's jobs and no others; `--admin` is the
    // deliberate administrative surface, and the operator's own shell
    // is unchanged. An incomplete or mismatched context is refused
    // rather than widened — the one thing it must never do.
    let scope = match crate::resolve_scope(
        "wirk atlas cancel",
        &estate,
        flag_value(rest, "--requesting-work"),
        rest.iter().any(|arg| arg == "--admin"),
    ) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk atlas cancel: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk atlas cancel: {note}");
    }
    let list = rest.iter().any(|arg| arg == "--list");
    let all = rest.iter().any(|arg| arg == "--all");
    let job = flag_value(rest, "--job");
    let source = flag_value(rest, "--source");
    let named = usize::from(list)
        + usize::from(all)
        + usize::from(job.is_some())
        + usize::from(source.is_some());
    if named != 1 {
        eprintln!(
            "wirk atlas cancel needs exactly one of --list, --job <id>, --source <name> or \
             --all: a cancellation states its target, and there is no default target on purpose"
        );
        return ExitCode::from(1);
    }
    let target = match (list, all, job, source) {
        (true, _, _, _) => None,
        (_, true, _, _) => Some(CancelTarget::All),
        (_, _, Some(id), _) => Some(CancelTarget::Job(id)),
        (_, _, _, Some(alias)) => Some(CancelTarget::Source(alias)),
        _ => return atlas_usage(),
    };
    let wait_secs = match flag_value(rest, "--wait") {
        None => 0,
        Some(value) => match value.parse::<u64>() {
            Ok(seconds) => seconds,
            Err(_) => {
                eprintln!("--wait takes a whole number of seconds, not {value:?}");
                return ExitCode::from(1);
            }
        },
    };
    let json = is_json(rest);
    wirkd_client_call(
        &estate,
        &Request::atlas_cancel(AtlasCancelPayload {
            work: scope.requesting,
            target,
            reason: flag_value(rest, "--reason"),
            wait_secs,
        }),
        |result| {
            print_result(json, result, |result| {
                let outcome = result["outcome"].as_str().unwrap_or("?");
                println!("outcome {outcome}");
                // Which surface answered. Without this an empty scoped
                // listing reads as an idle estate, which is exactly the
                // wrong conclusion to hand someone.
                if let Some(applied) = result["scope"].as_str() {
                    println!("scope {applied}");
                }
                if let Some(running) = result["running"].as_array() {
                    println!("running {}", running.len());
                    for job in running {
                        println!(
                            "  {} {} scope {} signalled {}",
                            job["job_id"].as_str().unwrap_or("?"),
                            job["verb"].as_str().unwrap_or("?"),
                            job["scope"].as_str().unwrap_or("?"),
                            job["cancel_signalled"].as_bool().unwrap_or(false)
                        );
                    }
                }
                if let Some(acknowledged) = result["acknowledged"].as_array() {
                    println!("acknowledged {}", acknowledged.len());
                    for job in acknowledged {
                        println!(
                            "  {} {} scope {} running_millis {}",
                            job["job_id"].as_str().unwrap_or("?"),
                            job["verb"].as_str().unwrap_or("?"),
                            job["scope"].as_str().unwrap_or("?"),
                            job["running_millis"].as_i64().unwrap_or(0)
                        );
                    }
                }
                // Completion is a separate line because it is a separate
                // fact: `null` means unobserved, not "finished".
                match result["completed"].as_array() {
                    Some(completed) => println!(
                        "completed {} still_running {}",
                        completed.len(),
                        result["still_running"].as_array().map_or(0, Vec::len)
                    ),
                    None => println!(
                        "completed unobserved (signalling is not stopping; re-run with --list \
                         or --wait to observe it)"
                    ),
                }
                if let Some(detail) = result["detail"].as_str() {
                    println!("{detail}");
                }
            });
        },
    )
}

fn status_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "status",
        rest,
        &[ESTATE, JSON, ("--source", true), ("--work", true)],
    ) {
        return code;
    }
    let Some(estate) = flag_value(rest, "--estate") else {
        return atlas_usage();
    };
    let source = flag_value(rest, "--source");
    let work = flag_value(rest, "--work").map(WorkId);
    let json = is_json(rest);
    wirkd_client_call(
        &estate,
        &Request::atlas_status(AtlasStatusPayload { source, work }),
        |result| {
            print_result(json, result, |result| {
                let sources = result["sources"].as_array().cloned().unwrap_or_default();
                println!(
                    "publication_revision {} sources_total {} sources_shown {} work_scoped {}",
                    result["publication_revision"].as_u64().unwrap_or(0),
                    result["sources_total"].as_u64().unwrap_or(0),
                    sources.len(),
                    result["work_scoped"].as_bool().unwrap_or(false)
                );
                for source in sources {
                    println!(
                        "  {} published {}",
                        source["membership"]["alias"].as_str().unwrap_or("?"),
                        source["published_generation"]["generation"]
                            .as_str()
                            .unwrap_or("none")
                    );
                    // P3 W4 A: staged/selected and each edition's actual
                    // verification state, in plain text as well as
                    // `--json` — the same honesty rule W3 applied to
                    // admission and coverage.
                    //
                    // W4-LIFECYCLE-CORRECTION.md item 1: the plain-text
                    // summary is what a caller actually reads, so the
                    // availability *state* and its reason are printed
                    // here and not only in `--json`. `available` is the
                    // derived answer, identical to the one `--json` and
                    // `search --semantic requested` give.
                    let semantic = &source["semantic"];
                    println!(
                        "    semantic selected {} available {} state {} editions {}",
                        semantic["selected"].as_str().unwrap_or("none"),
                        semantic["selected_available"]
                            .as_bool()
                            .map(|available| available.to_string())
                            .unwrap_or_else(|| "-".into()),
                        semantic["availability"]["state"].as_str().unwrap_or("?"),
                        semantic["editions_total"].as_u64().unwrap_or(0)
                    );
                    if let Some(detail) = semantic["availability"]["detail"].as_str() {
                        println!("      reason {detail}");
                    }
                    for edition in semantic["editions"].as_array().cloned().unwrap_or_default() {
                        println!(
                            "      {} {} {} {} rows {} model {} provenance {}",
                            edition["edition"].as_str().unwrap_or("?"),
                            edition["state"].as_str().unwrap_or("?"),
                            edition["verification"]["state"].as_str().unwrap_or("?"),
                            // Over the generation this source publishes
                            // now, or retained evidence of one it no
                            // longer does.
                            if edition["current"].as_bool().unwrap_or(false) {
                                "current"
                            } else {
                                "superseded"
                            },
                            edition["vectors"]["rows"].as_u64().unwrap_or(0),
                            edition["model"]["consumed"]["digest"]
                                .as_str()
                                .unwrap_or("-"),
                            // `unreported` is the honest answer for a
                            // backend that enumerated no environment, and
                            // for every edition built before this record
                            // existed. Among the ones that did report,
                            // the word is the *coverage* of what they
                            // measured — `complete`, `partial`, or
                            // `unmeasured` for a record written before
                            // loaded modules were measured at all — so
                            // that a 15-of-16 report and a 15-of-15 one
                            // stop rendering identically
                            // (`W4-PRODUCER-PROVENANCE-CORRECTION.md`
                            // item 2).
                            match edition["backend"]["environment"]["state"].as_str() {
                                Some("reported") =>
                                    edition["backend"]["environment"]["coverage"]["state"]
                                        .as_str()
                                        .unwrap_or("unmeasured"),
                                other => other.unwrap_or("unreported"),
                            }
                        );
                        // The reason a coverage is partial is the whole
                        // point of saying it is partial, so it prints
                        // here rather than only in `--json`.
                        if let Some(detail) =
                            edition["backend"]["environment"]["coverage"]["detail"].as_str()
                        {
                            println!("        provenance gap {detail}");
                        }
                    }
                }
            });
        },
    )
}

fn search_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "search",
        rest,
        &[
            ESTATE,
            JSON,
            ("--work", true),
            ("--query", true),
            ("--source", true),
            ("--semantic", true),
            ("--semantic-backend", true),
            ("--semantic-backend-arg", true),
            ("--semantic-model", true),
            ("--family", true),
            ("--limit", true),
            ("--capacity", true),
            ("--continue", true),
            ("--admin", false),
        ],
    ) {
        return code;
    }
    let Some(query) = flag_value(rest, "--query") else {
        return atlas_usage();
    };
    // One resolver for every reading verb; see `reading_scope`. Scope is
    // still decided daemon-side — this settles only what is asked.
    let (estate, work) = match reading_scope("wirk atlas search", rest) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    let json = is_json(rest);
    let source = flag_value(rest, "--source");
    let semantic = flag_value(rest, "--semantic");
    let families = flag_values(rest, "--family");
    let limit = flag_value(rest, "--limit").and_then(|value| value.parse::<usize>().ok());
    // Ruling 0171: how many ranked results this query's answer *consists
    // of*, as against `--limit`, which is how many of them one page
    // shows. Omitted, the capacity is the requested `--limit`, so an
    // ordinary search is the native ranking at the number of results it
    // asked for. A value that is not a number is refused here rather than
    // silently dropped: a caller who names a capacity is asking for a
    // specific result set, and quietly running a different one would be
    // exactly the substitution this policy exists to end.
    let capacity = match flag_value(rest, "--capacity") {
        None => None,
        Some(value) => match value.parse::<u64>() {
            Ok(parsed) => Some(parsed),
            Err(_) => {
                eprintln!(
                    "wirk atlas search: --capacity takes a whole number of results (1 to {}); \
                     it is this query's result capacity, not the number of results one page \
                     shows, which is --limit",
                    wirk_atlas::CAPACITY_MAX
                );
                return ExitCode::from(1);
            }
        },
    };
    let continuation = flag_value(rest, "--continue");
    wirkd_client_call(
        &estate,
        &Request::atlas_search(AtlasSearchPayload {
            work,
            query,
            source,
            semantic,
            families,
            limit,
            capacity,
            continuation,
            semantic_backend: flag_value(rest, "--semantic-backend"),
            semantic_backend_args: flag_values(rest, "--semantic-backend-arg"),
            semantic_model: flag_value(rest, "--semantic-model"),
        }),
        |result| {
            print_result(json, result, |result| {
                let hits = result["hits"].as_array().cloned().unwrap_or_default();
                // P3 W3 correction (ruling 0093, VERDICT.md L1): a
                // denial and a genuine no-match rendered identically in
                // plain text before — the admission/coverage state is
                // shown here, not only reachable via --json.
                println!(
                    "hits {} admission {} coverage {} budget {} semantic {} ranking {}",
                    hits.len(),
                    result["admission"],
                    result["coverage"],
                    result["budget"],
                    result["semantic"]["status"].as_str().unwrap_or("?"),
                    result["ranking"]["mode"].as_str().unwrap_or("?")
                );
                // P3 W4 B (0104's W1 limit, SEMANTIC-LIFECYCLE-LIMITS.md):
                // the plain surface is the one a human actually reads, so
                // the *reason* semantic use is unavailable or degraded —
                // and the fact that these hits are lexical instead — is
                // printed here, not left reachable only through --json.
                // The reason text is the same string the JSON carries;
                // both come from one place, so they cannot drift.
                if let Some(reason) = result["semantic"]["reason"].as_str() {
                    println!("  semantic reason {reason}");
                }
                // Ruling 0292 (`p5-foundation-use/USE.md` finding 4):
                // this search reads the estate's *current* publication.
                // When the calling Work's own delivered World was
                // captured at a different one, both vectors are in hand
                // and the difference is said here rather than left for
                // an actor to notice by reading two surfaces. It is a
                // disclosure and nothing else: the World is not
                // refreshed, the search is not pinned back, and current
                // discovery stays available.
                if result["captured_basis"]["state"].as_str() == Some("diverges") {
                    let basis = &result["captured_basis"];
                    println!(
                        "  captured basis diverges: this Run's World was captured at publication \
                         revision {}, these hits were read at {}",
                        basis["captured_publication_revision"].as_u64().unwrap_or(0),
                        basis["current_publication_revision"].as_u64().unwrap_or(0),
                    );
                    for entry in basis["differing_generations"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                    {
                        println!(
                            "    {} captured {} current {}",
                            entry["membership"].as_str().unwrap_or("?"),
                            entry["captured"]
                                .as_str()
                                .unwrap_or("not in the captured vector"),
                            entry["current"].as_str().unwrap_or("?"),
                        );
                    }
                }
                // A ranking that actually happened names what did it.
                if let Some(application) = result["ranking"]["application"].as_object() {
                    let number = |key: &str| {
                        application
                            .get(key)
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0)
                    };
                    println!(
                        "  ranked by {} over {} admitted rows, {} of {} editions",
                        application
                            .get("native")
                            .and_then(|value| value.as_str())
                            .unwrap_or("?"),
                        number("rows_ranked"),
                        result["ranking"]["editions"]
                            .as_array()
                            .map(|editions| editions.len())
                            .unwrap_or(0),
                        result["ranking"]["editions"]
                            .as_array()
                            .map(|editions| editions.len())
                            .unwrap_or(0),
                    );
                    // Ruling 0171, on the surface a human actually reads:
                    // what this query's result capacity was, where the
                    // number came from, and — in words, not a token —
                    // whether the result set filled that capacity or ran
                    // out inside it. Neither sentence claims anything
                    // about what the estate holds beyond this query.
                    println!(
                        "  result capacity {} ({}, at most {} per query under {}), returned {} \
                         ranked result(s); {}",
                        number("capacity"),
                        application
                            .get("capacity_source")
                            .and_then(|value| value.as_str())
                            .unwrap_or("?"),
                        number("capacity_max"),
                        application
                            .get("capacity_policy")
                            .and_then(|value| value.as_str())
                            .unwrap_or("?"),
                        number("result_rows"),
                        if application
                            .get("capacity_reached")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false)
                        {
                            "the result set filled this query's capacity, so relevant results may \
                             exist beyond it — asking for a deeper result set is a new query at a \
                             larger --capacity, not a continuation of this one"
                        } else {
                            "this query's result set is exhausted at this capacity, which says \
                             nothing about what else the admitted sources hold"
                        }
                    );
                    // The measured half of "ranked by …". `native` above
                    // is the child's own claim; this is the executable the
                    // product opened and digested, how many argv tokens it
                    // ran it with, and how much of what loaded inside it
                    // the record actually covers. A reader who only ever
                    // sees the plain surface still learns that the version
                    // string is not the identity.
                    if let Some(producer) = application.get("producer").and_then(|p| p.as_object())
                    {
                        let environment = &producer["environment"];
                        println!(
                            "  query implementation {} {} argv {} token(s), environment {}{}, \
                             identity {}, basis {}",
                            producer["program"]["canonical"].as_str().unwrap_or("?"),
                            short(producer["program"]["digest"].as_str().unwrap_or("?")),
                            producer["argv"].as_array().map(Vec::len).unwrap_or(0),
                            environment["state"].as_str().unwrap_or("?"),
                            environment["coverage"]["state"]
                                .as_str()
                                .map(|state| format!(" (coverage {state})"))
                                .unwrap_or_default(),
                            short(producer["digest"].as_str().unwrap_or("?")),
                            producer["basis"].as_str().unwrap_or("?"),
                        );
                        // O2: the plain reader was told "coverage partial"
                        // and left to guess what was unaccounted for, and
                        // is now told which packages. And a
                        // configuration-only basis is stated in words,
                        // because it is the one that changes what the
                        // caller can do next: this answer is complete and
                        // usable, and its continuation will be refused.
                        if let Some(detail) = environment["coverage"]["detail"].as_str() {
                            println!("    coverage detail: {detail}");
                        }
                        // Both bases now print their sentence, not just
                        // the weak one: what an `implementation_measured`
                        // pin covers — the modules the backend reported,
                        // and nothing else in the process — is exactly
                        // the thing a plain reader was previously left to
                        // read as a guarantee over the whole ranker
                        // (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(b)).
                        if let Some(detail) = producer["basis_detail"].as_str() {
                            println!("    basis: {detail}");
                        }
                    }
                }
                if result["coverage"]["continuation_unrecoverable"]
                    .as_bool()
                    .unwrap_or(false)
                {
                    println!(
                        "  this continuation cannot be reproduced; no page was returned and \
                         nothing was restarted"
                    );
                }
                // Ruling 0135 C4-R12: the plain surface is the one a
                // human actually reads, so "part of what you are
                // searching never became searchable" is a sentence here,
                // not a JSON flag. It names no path and no source: the
                // detail is `atlas status` for a source this caller is
                // already admitted to.
                if result["coverage"]["source_extraction_incomplete"]
                    .as_bool()
                    .unwrap_or(false)
                {
                    println!(
                        "  part of an admitted source could not be extracted into anything \
                         searchable at the generation this answer read, so those bytes are in \
                         the source and in no index here; see `wirk atlas status --source \
                         <name>` for the coverage counts of a source you are admitted to"
                    );
                }
                for hit in hits {
                    println!(
                        "  {} {}:{}-{} rev {} score {}",
                        hit["source"].as_str().unwrap_or("?"),
                        hit["path"].as_str().unwrap_or("?"),
                        hit["line_start"].as_u64().unwrap_or(0),
                        hit["line_end"].as_u64().unwrap_or(0),
                        hit["revision"].as_str().unwrap_or("?"),
                        hit["score"].as_f64().unwrap_or(0.0)
                    );
                    // The snippet is cut for presentation; a reader who
                    // never passes --json is still told the unit is
                    // bigger than what was shown, and that the
                    // coordinate above addresses all of it.
                    if hit["snippet_truncated"].as_bool().unwrap_or(false) {
                        println!(
                            "    snippet cut for display; the unit is {} bytes and the \
                             coordinate above names all of it",
                            hit["unit_bytes"].as_u64().unwrap_or(0)
                        );
                    }
                    // Ruling 0142: when the shown bytes are a window
                    // chosen around the query's own matches, the plain
                    // reader is told which lines those are and that the
                    // window has a coordinate of its own — otherwise the
                    // line range printed above (the whole unit) would
                    // read as the range that was displayed.
                    if let Some(terms) = hit["evidence"]["matched_terms"].as_array() {
                        let named = terms
                            .iter()
                            .filter_map(|term| term.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!(
                            "    shown: lines {}-{} of that unit, around {}{}",
                            hit["evidence"]["line_start"].as_u64().unwrap_or(0),
                            hit["evidence"]["line_end"].as_u64().unwrap_or(0),
                            if named.is_empty() {
                                "the match".to_string()
                            } else {
                                named
                            },
                            if hit["evidence"]["whole_match_shown"]
                                .as_bool()
                                .unwrap_or(true)
                            {
                                ""
                            } else {
                                "; the match itself is wider than the display budget and is cut"
                            }
                        );
                        if let Some(coordinate) = hit["evidence"]["coordinate"].as_str() {
                            println!("    shown coordinate {coordinate}");
                        }
                    }
                }
                if let Some(token) = result["continuation"].as_str() {
                    println!("continuation {token}");
                }
            });
        },
    )
}

/// `wirk atlas semantic build|select` (P3 W4 A,
/// `W4-PUBLIC-LIFECYCLE-BUILD.md`). Two verbs, deliberately not one:
/// building stages an immutable edition nothing reads, selecting is the
/// separate atomic publication. `--backend`/`--model` are the whole
/// portability boundary — the product ships no model name, no cache path
/// and no interpreter, and records exactly what it was handed.
fn semantic_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("build") => semantic_build_command(&rest[1..]),
        Some("select") => semantic_select_command(&rest[1..]),
        _ => atlas_usage(),
    }
}

fn semantic_build_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "semantic build",
        rest,
        &[
            ESTATE,
            JSON,
            ("--source", true),
            ("--generation", true),
            ("--backend", true),
            ("--backend-arg", true),
            ("--model", true),
            ("--chunker", true),
            ("--requesting-work", true),
            ("--admin", false),
        ],
    ) {
        return code;
    }
    let (Some(estate), Some(source), Some(generation), Some(backend), Some(model)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--source"),
        flag_value(rest, "--generation"),
        flag_value(rest, "--backend"),
        flag_value(rest, "--model"),
    ) else {
        return atlas_usage();
    };
    // Ruling 0251 F4: a build starts a real backend child that an
    // operator can later cancel, so it records who asked for it. Same
    // resolution as every other scoped verb: the actor's own Work
    // inside a context, `--admin` for an administrative build, and the
    // operator's shell unchanged.
    let scope = match crate::resolve_scope(
        "wirk atlas semantic build",
        &estate,
        flag_value(rest, "--requesting-work"),
        rest.iter().any(|arg| arg == "--admin"),
    ) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk atlas semantic build: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk atlas semantic build: {note}");
    }
    let json = is_json(rest);
    call_expecting_outcome(
        &estate,
        &Request::atlas_semantic_build(AtlasSemanticBuildPayload {
            source,
            generation,
            backend,
            backend_args: flag_values(rest, "--backend-arg"),
            model,
            chunker: flag_value(rest, "--chunker"),
            work: scope.requesting,
        }),
        &["staged"],
        |result| {
            print_result(json, result, |result| {
                println!(
                    "outcome {} edition {} rows {} dimensions {} model {}",
                    result["outcome"].as_str().unwrap_or("?"),
                    result["edition"]["edition"].as_str().unwrap_or("-"),
                    result["edition"]["vectors"]["rows"].as_u64().unwrap_or(0),
                    result["edition"]["vectors"]["dimensions"]
                        .as_u64()
                        .unwrap_or(0),
                    result["edition"]["model"]["consumed"]["digest"]
                        .as_str()
                        .unwrap_or("-"),
                );
                // P3 W4 B: what the rows are, what ranks them, and what
                // they honestly do not cover — in plain text, not only in
                // `--json`. An edition that skipped resources says so
                // here rather than leaving a caller to discover it.
                let edition = &result["edition"];
                if let Some(retrieval) = edition["retrieval"].as_object() {
                    println!(
                        "  chunks {} chunker {} retrieval {}",
                        retrieval
                            .get("chunking")
                            .and_then(|value| value.as_str())
                            .unwrap_or("?"),
                        edition["chunker"]["chunks"]["implementation"]
                            .as_str()
                            .unwrap_or("generation units"),
                        retrieval
                            .get("digest")
                            .and_then(|value| value.as_str())
                            .unwrap_or("-"),
                    );
                    // O1: the parse trees these boundaries came out of
                    // were produced by shared libraries the version
                    // string above does not pin. What was actually
                    // digested — or that nothing was — is said here, in
                    // plain text, and a library whose bytes the
                    // provider's own archive does not declare is named.
                    let grammars = &edition["chunker"]["chunks"]["grammars"];
                    match grammars["state"].as_str() {
                        Some("measured") => {
                            let libraries = grammars["libraries"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default();
                            println!(
                                "  grammars {} shared librar{} digested from {}",
                                libraries.len(),
                                if libraries.len() == 1 { "y" } else { "ies" },
                                grammars["cache_root"].as_str().unwrap_or("?"),
                            );
                            for library in &libraries {
                                if library["declaration"]["state"].as_str() == Some("undeclared") {
                                    println!(
                                        "    undeclared {} — {}",
                                        library["file"]["canonical"].as_str().unwrap_or("?"),
                                        library["declaration"]["detail"].as_str().unwrap_or("?"),
                                    );
                                }
                            }
                            for entry in grammars["uncovered"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default()
                            {
                                println!(
                                    "    uncovered {} — {}",
                                    entry["name"].as_str().unwrap_or("?"),
                                    entry["reason"].as_str().unwrap_or("?"),
                                );
                            }
                        }
                        Some(state @ ("none_loaded" | "unavailable")) => println!(
                            "  grammars {state}: {}",
                            grammars["reason"].as_str().unwrap_or("?")
                        ),
                        _ => println!(
                            "  grammars unreported: this build measured no parser shared library"
                        ),
                    }
                }
                let coverage = &edition["coverage"];
                if coverage["resources_indexed"].as_u64().unwrap_or(0) > 0 {
                    println!(
                        "  coverage {} of {} indexed resources produced rows, {} of {} bytes \
                         addressed",
                        coverage["resources_with_rows"].as_u64().unwrap_or(0),
                        coverage["resources_indexed"].as_u64().unwrap_or(0),
                        coverage["covered_bytes"].as_u64().unwrap_or(0),
                        coverage["indexed_bytes"].as_u64().unwrap_or(0),
                    );
                    for (label, key) in [
                        ("no rows", "resources_without_rows"),
                        ("unmapped", "resources_unmapped"),
                    ] {
                        for entry in coverage[key].as_array().cloned().unwrap_or_default() {
                            println!(
                                "    {label} {} — {}",
                                entry["name"].as_str().unwrap_or("?"),
                                entry["reason"].as_str().unwrap_or("?")
                            );
                        }
                    }
                }
                if let Some(detail) = result["detail"].as_str() {
                    println!("detail {detail}");
                }
            });
        },
    )
}

fn semantic_select_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "semantic select",
        rest,
        &[ESTATE, JSON, ("--source", true), ("--edition", true)],
    ) {
        return code;
    }
    let (Some(estate), Some(source), Some(edition)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--source"),
        flag_value(rest, "--edition"),
    ) else {
        return atlas_usage();
    };
    let json = is_json(rest);
    call_expecting_outcome(
        &estate,
        &Request::atlas_semantic_select(AtlasSemanticSelectPayload { source, edition }),
        &["selected"],
        |result| {
            print_result(json, result, |result| {
                println!(
                    "outcome {} edition {} publication_revision {}",
                    result["outcome"].as_str().unwrap_or("?"),
                    result["edition"]["edition"]
                        .as_str()
                        .unwrap_or(result["edition"].as_str().unwrap_or("-")),
                    result["publication_revision"].as_u64().unwrap_or(0),
                );
                if let Some(detail) = result["detail"].as_str() {
                    // A refused replacement must say what still stands.
                    println!(
                        "detail {detail}\nstill selected {}",
                        result["selected"].as_str().unwrap_or("none")
                    );
                }
            });
        },
    )
}

/// `wirk atlas resolve [--estate <root>] [--work <id>] --coordinate
/// <encoded>`: the verb `wirk world show` prints under every bound item,
/// and the one instruction W-C1 gives a fresh actor for closing the
/// loop.
///
/// It closes it because `--estate` and `--work` fall back to the
/// injected triple, by exactly the rule `status`/`watch` already run on
/// (`resolve_scope`, ruling 0117): the actor's own context resolves the
/// pair, an explicit flag always wins over it, a half-injected
/// environment is refused rather than widened, and outside an actor
/// context nothing changes at all. Before this, the printed command
/// failed with the usage banner inside a valid pane (ruling 0126, F3).
///
/// The fallback supplies the **pair**. `--estate` named explicitly is an
/// operator invocation and keeps the operator's meaning for an omitted
/// `--work`, so no existing command line changes what it asks for; and
/// when the triple is what resolved the estate, `--work` is always the
/// triple's Work, never the administrative read. Scope itself is still
/// decided daemon-side: a coordinate this Work may not read is refused
/// there, and possessing one confers nothing.
fn resolve_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "resolve",
        rest,
        &[
            ESTATE,
            JSON,
            ("--work", true),
            ("--admin", false),
            ("--coordinate", true),
        ],
    ) {
        return code;
    }
    let Some(coordinate) = flag_value(rest, "--coordinate") else {
        return atlas_usage();
    };
    let (estate, work) = match reading_scope("wirk atlas resolve", rest) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    let json = is_json(rest);
    call_expecting_outcome(
        &estate,
        &Request::atlas_resolve(AtlasResolvePayload { work, coordinate }),
        &["resolved"],
        |result| {
            print_result(json, result, |result| {
                println!(
                    "outcome {} {}:{}-{}",
                    result["outcome"].as_str().unwrap_or("?"),
                    result["path"].as_str().unwrap_or("?"),
                    result["line_start"].as_u64().unwrap_or(0),
                    result["line_end"].as_u64().unwrap_or(0)
                );
                // Ruling 0292 (`p5-foundation-use/USE.md` finding 3): an
                // unresolvable coordinate printed `outcome unavailable
                // ?:0-0` and dropped the one thing that says *why* —
                // which `--json` was carrying all along, from the same
                // field. The plain surface is the one an actor reads, so
                // the reason prints here too. Nothing is re-derived: it
                // is the `detail` this reply already carries, so the two
                // surfaces cannot drift.
                if let Some(detail) = result["detail"].as_str() {
                    println!("  reason {detail}");
                }
                // Said once, where a reader meets the limit: what the
                // World retains is the captured generation manifest and
                // the delivered excerpt, never the source bytes. A
                // coordinate that no longer resolves is not evidence of
                // loss by this estate (ruling 0270) — the original
                // changed or went away, and a validated Claim's own
                // artifact bytes are the thing that is retained.
                if result["outcome"].as_str() == Some("unavailable") {
                    println!(
                        "  retention a delivered World retains this coordinate's identity and \
                         its delivered excerpt, not the source bytes; full bytes are retained \
                         only by a validated Claim's own artifact"
                    );
                }
            });
        },
    )
}

fn relate_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "relate",
        rest,
        &[
            ESTATE,
            JSON,
            ("--work", true),
            ("--kind", true),
            ("--from", true),
            ("--to", true),
            ("--evidence", true),
            ("--run", true),
            ("--world", true),
        ],
    ) {
        return code;
    }
    let (Some(estate), Some(work), Some(kind), Some(from), Some(to)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--work"),
        flag_value(rest, "--kind"),
        flag_value(rest, "--from"),
        flag_value(rest, "--to"),
    ) else {
        return atlas_usage();
    };
    let evidence = flag_values(rest, "--evidence");
    if evidence.is_empty() {
        return atlas_usage();
    }
    let json = is_json(rest);
    wirkd_client_call(
        &estate,
        &Request::atlas_relate(AtlasRelatePayload {
            work: WorkId(work),
            kind,
            from,
            to,
            evidence,
            run: flag_value(rest, "--run"),
            world: flag_value(rest, "--world"),
        }),
        |result| {
            print_result(json, result, |result| {
                println!(
                    "relationship {} producer {}",
                    result["id"].as_str().unwrap_or("?"),
                    result["producer"].as_str().unwrap_or("?")
                );
            });
        },
    )
}

/// `wirk atlas document [--estate <root>] [--work <id> | --admin]
/// --coordinate <encoded> [--asset <id> --output <path>] [--json]`.
///
/// The reader beside `resolve`. `resolve` answers with the bytes a
/// coordinate's span names, which for a document source is a span of its
/// Markdown rendering; this answers with what the document itself is —
/// headings, tables, lists, links, notes, equations — and with an
/// inventory of the assets it embeds. `--asset <id>` then reads one of
/// those assets by the id that inventory listed.
///
/// **`--output` is required for asset bytes, and that is the point.** An
/// embedded asset is binary. Printing it would put it into whatever
/// transcript or prompt the caller's output lands in, which is exactly
/// what this reader is supposed to make unnecessary: the bytes go to a
/// file the caller named, and what is printed is the descriptor —
/// id, media type, origin part, length and digest.
fn document_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "document",
        rest,
        &[
            ESTATE,
            JSON,
            ("--work", true),
            ("--admin", false),
            ("--coordinate", true),
            ("--asset", true),
            ("--output", true),
        ],
    ) {
        return code;
    }
    let Some(coordinate) = flag_value(rest, "--coordinate") else {
        return atlas_usage();
    };
    let asset = match flag_value(rest, "--asset") {
        None => None,
        Some(raw) => match raw.parse::<usize>() {
            Ok(id) => Some(id),
            Err(_) => {
                eprintln!(
                    "wirk atlas document: --asset {raw:?} is not an asset id; ids are the \
                     non-negative integers this document's own inventory lists"
                );
                return ExitCode::from(1);
            }
        },
    };
    let output = flag_value(rest, "--output");
    match (asset, &output) {
        (Some(_), None) => {
            eprintln!(
                "wirk atlas document: --asset also needs --output <path>; an embedded asset is \
                 binary and is written to a file rather than printed"
            );
            return ExitCode::from(1);
        }
        (None, Some(_)) => {
            eprintln!(
                "wirk atlas document: --output is only for --asset <id>; without one this verb \
                 answers with structure and an asset inventory, and writes no file"
            );
            return ExitCode::from(1);
        }
        _ => {}
    }
    let (estate, work) = match reading_scope("wirk atlas document", rest) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    let json = is_json(rest);
    // **The delivery is part of the answer.** A validated daemon reply
    // says the asset was read, not that it reached the caller's disk, and
    // the two can disagree: an unwritable directory, a full filesystem, a
    // destination that is not a file. Recorded here and folded into the
    // exit code below, because the reply-shaped exit code alone would
    // report success for an asset that was never delivered.
    let delivery: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let code = call_expecting_outcome(
        &estate,
        &Request::atlas_document(AtlasDocumentPayload {
            work,
            coordinate,
            asset,
        }),
        &["read", "asset"],
        |result| {
            if let (Some(path), Some(hex)) = (&output, result["bytes_hex"].as_str()) {
                match decode_hex(hex) {
                    Some(bytes) => {
                        if let Err(error) = write_asset(path, &bytes) {
                            *delivery.borrow_mut() = Some(format!("writing {path}: {error}"));
                            return;
                        }
                    }
                    None => {
                        *delivery.borrow_mut() =
                            Some("the daemon's asset bytes were malformed".into());
                        return;
                    }
                }
            }
            // `bytes_hex` is transport, not an answer: it is never
            // printed, in `--json` or out of it.
            let mut shown = result.clone();
            if let Some(object) = shown.as_object_mut() {
                object.remove("bytes_hex");
                if let Some(path) = &output {
                    object.insert("written".into(), serde_json::Value::String(path.clone()));
                }
            }
            print_result(json, &shown, |shown| match shown["outcome"].as_str() {
                Some("asset") => {
                    println!(
                        "asset {} {} {} bytes {}",
                        shown["asset"]["id"],
                        shown["asset"]["media_type"].as_str().unwrap_or("?"),
                        shown["asset"]["bytes"],
                        short(shown["asset"]["digest"].as_str().unwrap_or("")),
                    );
                    println!("  from {}", shown["asset"]["origin_part"]);
                    if let Some(path) = &output {
                        println!("  written {path}");
                    }
                }
                // Every other outcome is a truthful non-answer — a
                // format with no document model, a resource read as text,
                // a parse that failed, an asset id this document does not
                // define — and each one names itself rather than being
                // rendered as an empty document.
                Some(outcome @ ("model_unavailable" | "not_a_document" | "failed" | "absent")) => {
                    println!("{outcome} {}", shown["path"].as_str().unwrap_or(""),);
                    if let Some(detail) = shown["detail"].as_str() {
                        println!("  {detail}");
                    }
                }
                _ => {
                    println!(
                        "document {} {}",
                        shown["path"].as_str().unwrap_or("?"),
                        shown["format"].as_str().unwrap_or("?"),
                    );
                    let structure = &shown["structure"];
                    println!(
                        "  blocks {} headings {} tables {} lists {} notes {} equations {} links \
                         {} images {}",
                        structure["blocks"],
                        structure["headings"].as_array().map_or(0, Vec::len),
                        structure["tables"].as_array().map_or(0, Vec::len),
                        structure["lists"],
                        structure["notes"],
                        structure["equations"],
                        structure["links"].as_array().map_or(0, Vec::len),
                        structure["images"],
                    );
                    for heading in structure["headings"].as_array().into_iter().flatten() {
                        println!(
                            "  h{} {}",
                            heading["level"],
                            heading["text"].as_str().unwrap_or("")
                        );
                    }
                    for asset in shown["assets"].as_array().into_iter().flatten() {
                        println!(
                            "  asset {} {} {} bytes  (--asset {} --output <path>)",
                            asset["id"],
                            asset["media_type"].as_str().unwrap_or("?"),
                            asset["bytes"],
                            asset["id"],
                        );
                    }
                }
            });
        },
    );
    if let Some(failure) = delivery.into_inner() {
        eprintln!("wirk atlas document: {failure}");
        // Nothing was delivered, so nothing here reports success — whatever
        // the daemon's own reply said about reading the asset.
        return ExitCode::from(2);
    }
    code
}

/// Write one asset's bytes to the destination the caller named, without
/// destroying whatever is already there if the write cannot finish.
///
/// `fs::write` truncates on open, so a failure part-way through leaves the
/// caller's existing file emptied — the one outcome an export must not
/// produce. The bytes go to a sibling temporary in the destination's own
/// directory and are renamed onto it, which is atomic on the same
/// filesystem: either the destination holds the whole asset or it holds
/// exactly what it held before. A destination whose directory cannot be
/// written fails at the temporary, before anything is touched.
///
/// Authority is the caller's own filesystem authority and nothing more:
/// this is an explicit export to a path the caller named, running as the
/// caller. There is no privileged write path here and none is added.
fn write_asset(destination: &str, bytes: &[u8]) -> std::io::Result<()> {
    let destination = Path::new(destination);
    let directory = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let name = destination.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "that destination does not name a file",
        )
    })?;
    let mut temporary = std::ffi::OsString::from(".");
    temporary.push(name);
    temporary.push(format!(".wirk-asset-{}", std::process::id()));
    let temporary = match directory {
        Some(directory) => directory.join(temporary),
        None => Path::new(&temporary).to_path_buf(),
    };
    std::fs::write(&temporary, bytes)?;
    match std::fs::rename(&temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// The inverse of the daemon's own `hex_encode`, for the one reply that
/// carries bytes. `None` for anything that is not an even-length run of
/// hex digits, so a malformed answer is refused rather than written to
/// the caller's file half-decoded.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in bytes.chunks(2) {
        let pair = std::str::from_utf8(pair).ok()?;
        out.push(u8::from_str_radix(pair, 16).ok()?);
    }
    Some(out)
}

/// `wirk atlas findings --estate <root> (--requesting-work <id> |
/// --admin [--rebuild]) [--json]` (W-B §7, corrected by
/// `W-B-DISCLOSURE-REPAIR.md`): lists the estate's derived Findings
/// index for one requesting Work, or — named explicitly — administers
/// it unscoped, optionally recreating it from every eligible journal
/// first. There is deliberately no default: the index discloses proof
/// targets and Application sources, and the shape that read them all
/// without naming anything is the defect this closes.
fn findings_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "findings",
        rest,
        &[
            ESTATE,
            JSON,
            ("--rebuild", false),
            ("--retire-preserved-index", false),
            ("--admin", false),
            ("--requesting-work", true),
        ],
    ) {
        return code;
    }
    let Some(estate) = flag_value(rest, "--estate") else {
        return atlas_usage();
    };
    let json = is_json(rest);
    let rebuild = rest.iter().any(|arg| arg == "--rebuild");
    let retire_preserved = rest.iter().any(|arg| arg == "--retire-preserved-index");
    let admin = rest.iter().any(|arg| arg == "--admin");
    let requester = flag_value(rest, "--requesting-work").map(wirk_core::WorkId);
    if admin == requester.is_some() {
        eprintln!(
            "wirk atlas findings: name exactly one of --requesting-work <id> (scoped) or --admin (unscoped)"
        );
        return ExitCode::from(2);
    }
    wirkd_client_call(
        &estate,
        &Request::atlas_findings(AtlasFindingsPayload {
            rebuild,
            retire_preserved,
            requester,
            admin,
        }),
        |result| {
            warn_if_index_incomplete("atlas findings", result);
            print_result(json, result, |result| {
                for pair in result["retired_index_copies"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    println!(
                        "retired {} as {} (its bytes are kept, not removed)",
                        pair["preserved"].as_str().unwrap_or("?"),
                        pair["retired"].as_str().unwrap_or("?")
                    );
                }
                let count = result["rows"]
                    .as_array()
                    .map(|rows| rows.len())
                    .unwrap_or(0);
                println!("{count} row(s)");
            });
        },
    )
}
