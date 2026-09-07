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
    AtlasAcquirePayload, AtlasPublishPayload, AtlasRefreshPayload, AtlasRelatePayload,
    AtlasResolvePayload, AtlasSearchPayload, AtlasSemanticBuildPayload, AtlasSemanticSelectPayload,
    AtlasStatusPayload, Reply, Request,
};
use crate::{flag_value, wirkd_client_call};
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
        _ => atlas_usage(),
    }
}

fn atlas_usage() -> ExitCode {
    eprintln!(
        "usage: wirk atlas acquire --estate <root> --source <name> --repository <path> --revision <ref> [--json] \
         | wirk atlas refresh --estate <root> --source <name> --revision <ref> [--json] \
         | wirk atlas publish --estate <root> --source <name> --generation <id> [--json] \
         | wirk atlas status --estate <root> [--source <name>] [--work <id>] [--json] \
         | wirk atlas resolve --estate <root> [--work <id>] --coordinate <encoded> [--json] \
         | wirk atlas search --estate <root> [--work <id>] --query <text> [--source <name>] [--semantic requested|disabled] [--family code|knowledge|config]... [--limit <n>] [--continue <token>] [--json] \
         | wirk atlas semantic build --estate <root> --source <name> --generation <id> --backend <path> [--backend-arg <arg>...] --model <dir> [--json] \
         | wirk atlas semantic select --estate <root> --source <name> --edition <id> [--json] \
         | wirk atlas relate --estate <root> --work <id> --kind governed_by --from <coordinate> --to <coordinate> --evidence <coordinate> [--evidence <coordinate>...] [--run <id>] [--world <hash>] [--json]"
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
            ("--family", true),
            ("--limit", true),
            ("--continue", true),
        ],
    ) {
        return code;
    }
    let (Some(estate), Some(query)) = (flag_value(rest, "--estate"), flag_value(rest, "--query"))
    else {
        return atlas_usage();
    };
    let json = is_json(rest);
    let work = flag_value(rest, "--work").map(WorkId);
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
        }),
        |result| {
            print_result(json, result, |result| {
                let hits = result["hits"].as_array().cloned().unwrap_or_default();
                // P3 W3 correction (ruling 0093, VERDICT.md L1): a
                // denial and a genuine no-match rendered identically in
                // plain text before — the admission/coverage state is
                // shown here, not only reachable via --json.
                println!(
                    "hits {} admission {} coverage {} budget {} semantic {}",
                    hits.len(),
                    result["admission"],
                    result["coverage"],
                    result["budget"],
                    result["semantic"]["status"].as_str().unwrap_or("?")
                );
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

fn resolve_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "resolve",
        rest,
        &[ESTATE, JSON, ("--work", true), ("--coordinate", true)],
    ) {
        return code;
    }
    let (Some(estate), Some(coordinate)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--coordinate"),
    ) else {
        return atlas_usage();
    };
    let json = is_json(rest);
    let work = flag_value(rest, "--work").map(WorkId);
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
