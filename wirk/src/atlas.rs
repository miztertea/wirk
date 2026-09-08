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
    AtlasAcquirePayload, AtlasFindingsPayload, AtlasPublishPayload, AtlasRefreshPayload,
    AtlasRelatePayload, AtlasResolvePayload, AtlasSearchPayload, AtlasSemanticBuildPayload,
    AtlasSemanticSelectPayload, AtlasStatusPayload, Reply, Request,
};
use crate::{ActorContext, actor_context, flag_value, warn_if_index_incomplete, wirkd_client_call};
use wirk_core::WorkId;

pub fn atlas_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("acquire") => acquire_command(&rest[1..]),
        Some("refresh") => refresh_command(&rest[1..]),
        Some("publish") => publish_command(&rest[1..]),
        Some("status") => status_command(&rest[1..]),
        Some("search") => search_command(&rest[1..]),
        Some("resolve") => resolve_command(&rest[1..]),
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
        "usage: wirk atlas acquire --estate <root> --source <name> --repository <path> --revision <ref> [--json] \
         | wirk atlas refresh --estate <root> --source <name> --revision <ref> [--json] \
         | wirk atlas publish --estate <root> --source <name> --generation <id> [--json] \
         | wirk atlas status --estate <root> [--source <name>] [--work <id>] [--json] \
         | wirk atlas resolve [--estate <root>] [--work <id>] --coordinate <encoded> [--json] \
         | wirk atlas search --estate <root> [--work <id>] --query <text> [--source <name>] [--semantic requested|disabled] [--semantic-backend <path>] [--semantic-backend-arg <arg>...] [--semantic-model <dir>] [--family code|knowledge|config]... [--limit <n>] [--continue <token>] [--json] \
         | wirk atlas semantic build --estate <root> --source <name> --generation <id> --backend <path> [--backend-arg <arg>...] --model <dir> [--chunker units|native] [--json] \
         | wirk atlas semantic select --estate <root> --source <name> --edition <id> [--json] \
         | wirk atlas relate --estate <root> --work <id> --kind governed_by --from <coordinate> --to <coordinate> --evidence <coordinate> [--evidence <coordinate>...] [--run <id>] [--world <hash>] [--json] \
         | wirk atlas findings --estate <root> (--requesting-work <id> | --admin [--rebuild | --retire-preserved-index]) [--json]"
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
fn check_flags(verb: &str, rest: &[String], allowed: &[(&str, bool)]) -> Result<(), ExitCode> {
    let mut index = 0;
    while index < rest.len() {
        let arg = &rest[index];
        let Some((_, takes_value)) = allowed.iter().find(|(name, _)| name == arg) else {
            if arg.starts_with('-') {
                eprintln!(
                    "wirk atlas {verb}: unknown flag {arg}\n\
                     accepted flags: {}",
                    allowed
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            } else {
                eprintln!("wirk atlas {verb}: unexpected argument {arg}");
            }
            return Err(ExitCode::from(2));
        };
        index += 1;
        if *takes_value {
            if index >= rest.len() {
                eprintln!("wirk atlas {verb}: {arg} requires a value");
                return Err(ExitCode::from(2));
            }
            index += 1;
        }
    }
    Ok(())
}

const ESTATE: (&str, bool) = ("--estate", true);
const JSON: (&str, bool) = ("--json", false);

fn flag_values(rest: &[String], flag: &str) -> Vec<String> {
    rest.iter()
        .zip(rest.iter().skip(1))
        .filter(|(name, _)| name.as_str() == flag)
        .map(|(_, value)| value.clone())
        .collect()
}

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
            eprintln!("wirk wirkd: {} {}", error.code, error.message);
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
        ],
    ) {
        return code;
    }
    let (Some(estate), Some(source), Some(repository), Some(revision)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--source"),
        flag_value(rest, "--repository"),
        flag_value(rest, "--revision"),
    ) else {
        return atlas_usage();
    };
    let json = is_json(rest);
    call_expecting_outcome(
        &estate,
        &Request::atlas_acquire(AtlasAcquirePayload {
            source,
            repository,
            revision,
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

fn refresh_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "refresh",
        rest,
        &[ESTATE, JSON, ("--source", true), ("--revision", true)],
    ) {
        return code;
    }
    let (Some(estate), Some(source), Some(revision)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--source"),
        flag_value(rest, "--revision"),
    ) else {
        return atlas_usage();
    };
    let json = is_json(rest);
    call_expecting_outcome(
        &estate,
        &Request::atlas_refresh(AtlasRefreshPayload { source, revision }),
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
        &[ESTATE, JSON, ("--source", true), ("--generation", true)],
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
    let json = is_json(rest);
    wirkd_client_call(
        &estate,
        &Request::atlas_publish(AtlasPublishPayload { source, generation }),
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
            ("--continue", true),
        ],
    ) {
        return code;
    }
    let Some(query) = flag_value(rest, "--query") else {
        return atlas_usage();
    };
    // The same fallback `resolve` runs on (ruling 0126 F3, ruling 0117):
    // `--estate`/`--work` come from the injected triple when they are not
    // named, so the `fetch` line a stage projection prints under every
    // reachable handle runs verbatim inside a pane. An explicit
    // `--estate` is an operator invocation and keeps the operator's
    // meaning for an omitted `--work`; a half-injected environment is
    // refused rather than widened; outside an actor context nothing
    // changes. Scope is still decided daemon-side.
    let named_work = flag_value(rest, "--work").map(WorkId);
    let (estate, work) = match flag_value(rest, "--estate") {
        Some(estate) => (estate, named_work),
        None => match actor_context() {
            ActorContext::Present {
                estate_root,
                work_id,
            } => (estate_root, named_work.or(Some(WorkId(work_id)))),
            ActorContext::Partial { missing } => {
                eprintln!(
                    "wirk atlas search: --estate was not given and the injected context is \
                     incomplete ({}); name --estate <root> explicitly, or run inside a complete \
                     execution context",
                    missing.join(", ")
                );
                return ExitCode::from(1);
            }
            ActorContext::Absent => return atlas_usage(),
        },
    };
    let json = is_json(rest);
    let source = flag_value(rest, "--source");
    let semantic = flag_value(rest, "--semantic");
    let families = flag_values(rest, "--family");
    let limit = flag_value(rest, "--limit").and_then(|value| value.parse::<usize>().ok());
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
                // A ranking that actually happened names what did it.
                if let Some(application) = result["ranking"]["application"].as_object() {
                    println!(
                        "  ranked by {} over {} admitted rows, {} of {} editions, candidate \
                         limit {}{}",
                        application
                            .get("native")
                            .and_then(|value| value.as_str())
                            .unwrap_or("?"),
                        application
                            .get("rows_ranked")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0),
                        result["ranking"]["editions"]
                            .as_array()
                            .map(|editions| editions.len())
                            .unwrap_or(0),
                        result["ranking"]["editions"]
                            .as_array()
                            .map(|editions| editions.len())
                            .unwrap_or(0),
                        application
                            .get("candidate_limit")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0),
                        if application
                            .get("candidates_saturated")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false)
                        {
                            " (saturated: more candidates exist beyond it)"
                        } else {
                            ""
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
        &[ESTATE, JSON, ("--work", true), ("--coordinate", true)],
    ) {
        return code;
    }
    let Some(coordinate) = flag_value(rest, "--coordinate") else {
        return atlas_usage();
    };
    let named_work = flag_value(rest, "--work").map(WorkId);
    let (estate, work) = match flag_value(rest, "--estate") {
        Some(estate) => (estate, named_work),
        None => match actor_context() {
            ActorContext::Present {
                estate_root,
                work_id,
            } => (estate_root, named_work.or(Some(WorkId(work_id)))),
            // Half a triple names no identity, and reading it as "no
            // context" is the wider reading. Refused, never widened.
            ActorContext::Partial { missing } => {
                eprintln!(
                    "wirk atlas resolve: --estate was not given and the injected context is \
                     incomplete ({}); name --estate <root> explicitly, or run inside a complete \
                     execution context",
                    missing.join(", ")
                );
                return ExitCode::from(1);
            }
            ActorContext::Absent => return atlas_usage(),
        },
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
