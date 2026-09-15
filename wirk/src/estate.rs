//! `wirk estate storage` and `wirk estate clean` (P4.5 increment A,
//! ruling 0256).
//!
//! Two verbs, and a deliberately small surface: an operator asks what
//! the estate is keeping, then removes something specific from it. There
//! is no second configuration system — per-class soft limits live in the
//! `.wirk/resources.json` increment B already reads — and no new
//! dependency.
//!
//! Both take the same scope resolution every other Work-aware verb uses
//! (`crate::resolve_scope`, ruling 0117), so an actor context asks as its
//! own Work and the administrative surface is reached deliberately rather
//! than by leaving an argument off.

use std::process::ExitCode;

use crate::wirkd;
use crate::wirkd::Request;
use crate::{check_flags, flag_value, flag_values};

const ESTATE: (&str, bool) = ("--estate", true);
const JSON: (&str, bool) = ("--json", false);

pub(crate) fn estate_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("storage") => storage_command(&rest[1..]),
        Some("clean") => clean_command(&rest[1..]),
        Some("doctrine") => doctrine_command(&rest[1..]),
        _ => usage(),
    }
}

pub(crate) fn usage() -> ExitCode {
    eprintln!(
        "usage: wirk estate storage --estate <root> [--requesting-work <id> | --admin] [--json] \
         | wirk estate clean --estate <root> --class <{}> (--id <id>... | --all-unreferenced) \
         [--dry-run] [--admin] [--json] \
         | wirk estate doctrine list --estate <root> [--requesting-work <id> | --admin] [--json] \
         | wirk estate doctrine set --estate <root> --id <id> --path <file> [--version <label>] \
         [--repository <name>] --admin [--json] \
         | wirk estate doctrine remove --estate <root> --id <id> --admin [--json]",
        wirk_core::storage::CLEANABLE_CLASSES.join("|")
    );
    ExitCode::from(1)
}

/// `wirk estate doctrine` — the estate owner's explicit selection of the
/// scoped doctrine documents every applicable actor is reserved with.
///
/// Deliberately small, and deliberately explicit: one document at a
/// time, named by the owner, with the owner's own version label and the
/// owner's own scope. Wirk does not search for doctrine, does not
/// promote a nearby file into it, and does not turn something a source
/// retrieval surfaced into governing rules. Silence means no doctrine.
fn doctrine_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("list") => doctrine_call("list", &rest[1..]),
        Some("set") => doctrine_call("set", &rest[1..]),
        Some("remove") => doctrine_call("remove", &rest[1..]),
        _ => usage(),
    }
}

fn doctrine_call(action: &str, rest: &[String]) -> ExitCode {
    let verb = format!("wirk estate doctrine {action}");
    let mut allowed = vec![
        ESTATE,
        JSON,
        ("--requesting-work", true),
        ("--admin", false),
    ];
    match action {
        "set" => allowed.extend([
            ("--id", true),
            ("--path", true),
            ("--version", true),
            ("--repository", true),
        ]),
        "remove" => allowed.push(("--id", true)),
        _ => {}
    }
    if let Err(code) = check_flags(&verb, rest, &allowed) {
        return code;
    }
    let Some(estate) = flag_value(rest, "--estate") else {
        return usage();
    };
    let scope = match crate::resolve_scope(
        &verb,
        &estate,
        flag_value(rest, "--requesting-work"),
        rest.iter().any(|arg| arg == "--admin"),
    ) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("{verb}: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("{verb}: {note}");
    }
    let json = rest.iter().any(|arg| arg == "--json");

    let action = match action {
        "list" => wirkd::DoctrineAction::List,
        "set" => {
            let (Some(id), Some(path)) = (flag_value(rest, "--id"), flag_value(rest, "--path"))
            else {
                eprintln!("{verb}: --id <id> and --path <file> are both required");
                return ExitCode::from(1);
            };
            // Resolved here, in the caller's own shell, because that is
            // where a relative path means what the person typing it
            // meant. The daemon is a different process in a different
            // directory and must never be the one to guess.
            let path = match std::path::Path::new(&path).canonicalize() {
                Ok(resolved) => resolved.display().to_string(),
                Err(error) => {
                    eprintln!("{verb}: --path {path} could not be resolved: {error}");
                    return ExitCode::from(1);
                }
            };
            wirkd::DoctrineAction::Set {
                id,
                path,
                version: flag_value(rest, "--version"),
                repository: flag_value(rest, "--repository"),
            }
        }
        _ => {
            let Some(id) = flag_value(rest, "--id") else {
                eprintln!("{verb}: --id <id> is required");
                return ExitCode::from(1);
            };
            wirkd::DoctrineAction::Remove { id }
        }
    };

    crate::wirkd_client_call(
        &estate,
        &Request::estate_doctrine(wirkd::EstateDoctrinePayload {
            work: scope.requesting.clone(),
            action,
        }),
        |result| {
            if json {
                println!("{result}");
                return;
            }
            render_doctrine(result);
        },
    )
}

fn render_doctrine(result: &serde_json::Value) {
    let text = |key: &str| result[key].as_str().unwrap_or("").to_string();
    if let Some(documents) = result["documents"].as_array() {
        println!("estate {} ({} scope)", text("estate"), text("scope"));
        if documents.is_empty() {
            println!("no estate doctrine is declared");
        }
        for document in documents {
            let scope = document["repository"].as_str().map_or_else(
                || "estate-wide".to_string(),
                |name| format!("repository {name}"),
            );
            println!(
                "{:<24} version {:<16} {}",
                document["id"].as_str().unwrap_or(""),
                document["version"].as_str().unwrap_or(""),
                scope
            );
            if let Some(path) = document["path"].as_str() {
                println!("{:<24}   {path}", "");
            }
        }
    } else if result["removed"].as_bool().unwrap_or(false) {
        println!("removed {}", text("id"));
    } else {
        println!(
            "{} {} (version {}, sha256 {}, {} bytes)",
            if result["replaced"].as_bool().unwrap_or(false) {
                "replaced"
            } else {
                "declared"
            },
            text("id"),
            text("version"),
            text("digest"),
            result["bytes"].as_u64().unwrap_or(0)
        );
        println!("  {}", text("path"));
    }
    if !text("note").is_empty() {
        println!("{}", text("note"));
    }
}

/// `wirk estate storage` — what this estate owns, what still needs it,
/// and what it occupies. Reads only: it creates no directory, takes no
/// slot and removes nothing.
fn storage_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "estate storage",
        rest,
        &[
            ESTATE,
            JSON,
            ("--requesting-work", true),
            ("--admin", false),
        ],
    ) {
        return code;
    }
    let Some(estate) = flag_value(rest, "--estate") else {
        return usage();
    };
    let scope = match crate::resolve_scope(
        "wirk estate storage",
        &estate,
        flag_value(rest, "--requesting-work"),
        rest.iter().any(|arg| arg == "--admin"),
    ) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk estate storage: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk estate storage: {note}");
    }
    let json = rest.iter().any(|arg| arg == "--json");

    crate::wirkd_client_call(
        &estate,
        &Request::estate_storage(wirkd::EstateStoragePayload {
            work: scope.requesting.clone(),
        }),
        |result| {
            if json {
                println!("{result}");
                return;
            }
            render_storage(result);
        },
    )
}

fn render_storage(result: &serde_json::Value) {
    println!(
        "estate {} ({} scope)",
        result["estate"].as_str().unwrap_or(""),
        result["scope"].as_str().unwrap_or("")
    );
    println!(
        "{:<18} {:>12} {:>12} {:>8}  status",
        "class", "unique", "allocated", "items"
    );
    for class in result["classes"].as_array().unwrap_or(&Vec::new()) {
        let removable = class["removable_items"].as_u64().unwrap_or(0);
        let retained = class["retained_items"].as_u64().unwrap_or(0);
        let mut status = if class["cleanable"].as_bool().unwrap_or(false) {
            format!("{removable} unreferenced, {retained} retained")
        } else {
            "not removable here".to_string()
        };
        if class["over_soft_limit"].as_bool().unwrap_or(false) {
            status.push_str(&format!(
                "; OVER the {} byte soft limit (disclosure only)",
                class["soft_limit_bytes"].as_u64().unwrap_or(0)
            ));
        }
        if let Some(limit) = class["measurement_limit"].as_str() {
            status.push_str(&format!("; {limit}"));
        }
        println!(
            "{:<18} {:>12} {:>12} {:>8}  {}",
            class["class"].as_str().unwrap_or(""),
            class["unique_allocated_bytes"].as_u64().unwrap_or(0),
            class["allocated_bytes"].as_u64().unwrap_or(0),
            class["entries"].as_u64().unwrap_or(0),
            status
        );
    }
    println!(
        "estate total (each inode once): {} bytes across {} distinct inodes",
        result["estate_unique_allocated_bytes"]
            .as_u64()
            .unwrap_or(0),
        result["distinct_inodes"].as_u64().unwrap_or(0)
    );
    match result["available_bytes"].as_u64() {
        Some(available) => println!("filesystem free: {available} bytes"),
        None => println!(
            "filesystem free: unavailable ({})",
            result["available_bytes_unavailable"]
                .as_str()
                .unwrap_or("not reported")
        ),
    }
    for shared in result["host_shared"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "host-shared {} at {}: {} bytes — not charged to this estate ({})",
            shared["name"].as_str().unwrap_or(""),
            shared["path"].as_str().unwrap_or(""),
            shared["unique_allocated_bytes"].as_u64().unwrap_or(0),
            shared["note"].as_str().unwrap_or("")
        );
    }
    for source in result["sources"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "source {} -> {} (original; never copied here, never removable)",
            source["alias"].as_str().unwrap_or(""),
            source["locator"].as_str().unwrap_or("")
        );
    }
    println!(
        "none of these figures is a count of bytes a removal would return; see \
         measurement.not_a_reclaim_estimate in --json"
    );
    let retention = &result["retention"];
    if !retention["complete"].as_bool().unwrap_or(true) {
        println!("retention is INCOMPLETE, so nothing here is established as unreferenced:");
        for line in retention["unreadable"].as_array().unwrap_or(&Vec::new()) {
            println!("  {}", line.as_str().unwrap_or(""));
        }
        // Present only on a scoped read, and said out loud there: an
        // absent path in these lines is a withholding, not a report that
        // there was nothing to name (ruling 0260).
        if let Some(withheld) = retention["identities_withheld"].as_str() {
            println!("  ({withheld})");
        }
    }
}

/// `wirk estate clean` — explicit, guarded removal of optional
/// derivations. Administrative, one selector, `--dry-run`-able.
fn clean_command(rest: &[String]) -> ExitCode {
    if let Err(code) = check_flags(
        "estate clean",
        rest,
        &[
            ESTATE,
            JSON,
            ("--class", true),
            ("--id", true),
            ("--all-unreferenced", false),
            ("--dry-run", false),
            ("--requesting-work", true),
            ("--admin", false),
        ],
    ) {
        return code;
    }
    let Some(estate) = flag_value(rest, "--estate") else {
        return usage();
    };
    let Some(class) = flag_value(rest, "--class") else {
        return usage();
    };
    let scope = match crate::resolve_scope(
        "wirk estate clean",
        &estate,
        flag_value(rest, "--requesting-work"),
        rest.iter().any(|arg| arg == "--admin"),
    ) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk estate clean: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk estate clean: {note}");
    }
    let ids = flag_values(rest, "--id");
    let all_unreferenced = rest.iter().any(|arg| arg == "--all-unreferenced");
    let dry_run = rest.iter().any(|arg| arg == "--dry-run");
    let json = rest.iter().any(|arg| arg == "--json");

    crate::wirkd_client_call(
        &estate,
        &Request::estate_clean(wirkd::EstateCleanPayload {
            work: scope.requesting.clone(),
            class: class.clone(),
            ids,
            all_unreferenced,
            dry_run,
        }),
        |result| {
            if json {
                println!("{result}");
                return;
            }
            render_clean(result, dry_run, &class);
        },
    )
}

fn render_clean(result: &serde_json::Value, dry_run: bool, class: &str) {
    let empty = Vec::new();
    if dry_run {
        let selected = result["selected"].as_array().unwrap_or(&empty);
        println!("would remove {} item(s) from {class}:", selected.len());
        for item in selected {
            println!(
                "  {} {} ({} unique bytes)",
                item["id"].as_str().unwrap_or(""),
                item["path"].as_str().unwrap_or(""),
                item["unique_allocated_bytes"].as_u64().unwrap_or(0)
            );
        }
        println!(
            "estimate {} bytes — an estimate, not a promise of what a removal returns",
            result["estimated_unique_allocated_bytes"]
                .as_u64()
                .unwrap_or(0)
        );
    } else {
        let removed = result["removed"].as_array().unwrap_or(&empty);
        println!("removed {} item(s) from {class}:", removed.len());
        for item in removed {
            println!(
                "  {} {}",
                item["id"].as_str().unwrap_or(""),
                item["path"].as_str().unwrap_or("")
            );
        }
        for item in result["failed"].as_array().unwrap_or(&empty) {
            println!(
                "  FAILED {} {}: {}",
                item["id"].as_str().unwrap_or(""),
                item["path"].as_str().unwrap_or(""),
                item["error"].as_str().unwrap_or("")
            );
        }
        match result["reclaimed_observed_bytes"].as_u64() {
            Some(observed) => println!(
                "free space moved by {observed} bytes across this call — an observation, \
                 approximate, not a guarantee"
            ),
            None => println!(
                "reclaimed bytes not observed: {}",
                result["reclaimed_observed_unavailable"]
                    .as_str()
                    .unwrap_or("free space could not be read")
            ),
        }
        println!(
            "complete: {}",
            result["complete"].as_bool().unwrap_or(false)
        );
    }
    for item in result["refused"].as_array().unwrap_or(&empty) {
        println!(
            "  refused {}: {} {}",
            item["id"].as_str().unwrap_or(""),
            item["reason"].as_str().unwrap_or(""),
            item["retained_by"]
                .as_array()
                .map(|holders| holders
                    .iter()
                    .filter_map(|holder| holder.as_str())
                    .collect::<Vec<_>>()
                    .join("; "))
                .unwrap_or_default()
        );
    }
}
