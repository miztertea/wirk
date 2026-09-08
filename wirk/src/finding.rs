//! `wirk finding ...` (W-B, `knowledge/work/p3-world-loop/W-B-BUILD.md`,
//! corrected by `loop-b-prepare-correct/HANDOFF.md` and
//! `W-B-CONSTRUCTION-REVIEW.md`): five thin JSON-capable clients over
//! wirkd's own Finding verbs (`crate::wirkd::{FindingRaisePayload, ...}`).
//! This module parses argv and prints; `wirkd::server::handle_finding_*`
//! owns every actual admission/settlement/application decision.
//!
//! `raise` and `applied` read the injected pane triple from env, exactly
//! like `wirk claim` (§9: "pane, triple from env") — no `--estate` flag.
//! `applied` gained this in the W-B-CORRECT.md correction: recording a
//! durable `FindingApplied` (even the unverified `Attribution::Asserted`
//! path) requires a real, checked, current producer identity, never a
//! bare `--by` string from an arbitrary shell (the authority review's
//! own executed counterexample). `assert`/`settle`/`list` stay operator/
//! client calls, `--estate <root>` like `wirk atlas`.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use crate::wirkd::{
    FindingAppliedPayload, FindingAssertPayload, FindingListPayload, FindingRaisePayload,
    FindingSettlePayload, Reply, Request,
};
use crate::{TRIPLE_VARS, flag_value, wirkd_client_call};
use wirk_core::{ExecutionTriple, RunId, WorkId};

pub fn finding_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("raise") => raise_command(&rest[1..]),
        Some("assert") => assert_command(&rest[1..]),
        Some("settle") => settle_command(&rest[1..]),
        Some("applied") => applied_command(&rest[1..]),
        Some("list") => list_command(&rest[1..]),
        _ => finding_usage(),
    }
}

fn finding_usage() -> ExitCode {
    eprintln!(
        "usage: wirk finding raise --kind gap|contradicted_assumption|relationship|verified_outcome --scope work_local|estate_local --claim <text> [--evidence <ref>...] [--contradicts <ref>...] [--applies-to <coord>...] [--supersedes <id>] [--proposed-change <t>] [--obligation <id>@<edition>] [--confirmed-by work/<id>/finding/<id>] [--json] \
         \n    <ref> is <coord> | work/<work-id>/event/<event-id> | work/<work-id>/finding/<finding-id>; \
         a finding reference in --contradicts records this Work's own claim of disagreement with that exact record, and settles, publishes and changes nothing about it \
         \n | wirk finding assert --estate <root> --finding <id> --decision accepted|partially_accepted|rejected|deferred|superseded --by <name> (--requesting-work <id> | --admin) [--reason <t>] [--superseded-by <id>] [--json] \
         | wirk finding settle --estate <root> --finding <id> (--requesting-work <id> | --admin) [--json] \
         | wirk finding applied --finding <id> --source <alias> --revision <sha> --by <name> [--claim-run <run-id> [--claim-work <id>]] [--json] (pane, triple from env — no --estate) \
         | wirk finding list --estate <root> (--requesting-work <id> [--work <id>] | --admin [--work <id>]) [--json]"
    );
    ExitCode::from(1)
}

fn is_json(rest: &[String]) -> bool {
    rest.iter().any(|arg| arg == "--json")
}

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

/// The injected pane triple from env (`WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/
/// `WIRK_RUN_ID`), shared by every verb that must bind to a real, current
/// caller identity rather than trust a bare `--by` string
/// (W-B-CORRECT.md defect 3: "require current valid producing Work/Run/
/// World"). `wirkd` still performs the actual currency check; this only
/// reads what the pane already injected.
fn triple_from_env(command_name: &str) -> Result<ExecutionTriple, ExitCode> {
    let mut missing = Vec::new();
    let mut triple: std::collections::BTreeMap<&str, String> = std::collections::BTreeMap::new();
    for name in TRIPLE_VARS {
        match env::var(name).ok().filter(|v| !v.trim().is_empty()) {
            Some(value) => {
                triple.insert(name, value);
            }
            None => missing.push(name),
        }
    }
    if !missing.is_empty() {
        for name in &missing {
            eprintln!("wirk finding {command_name}: missing {name}");
        }
        return Err(ExitCode::from(1));
    }
    Ok(ExecutionTriple {
        estate_root: triple["WIRK_ESTATE_ROOT"].clone(),
        work_id: WorkId(triple["WIRK_WORK_ID"].clone()),
        run_id: RunId(triple["WIRK_RUN_ID"].clone()),
    })
}

/// `wirk finding raise`: the injected triple names the raising Run
/// (`WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`, same as `wirk
/// claim`); wirkd refuses one that is not current for its Waypoint.
fn raise_command(rest: &[String]) -> ExitCode {
    let (Some(kind), Some(scope), Some(claim)) = (
        flag_value(rest, "--kind"),
        flag_value(rest, "--scope"),
        flag_value(rest, "--claim"),
    ) else {
        return finding_usage();
    };
    let json = is_json(rest);

    let triple = match triple_from_env("raise") {
        Ok(triple) => triple,
        Err(code) => return code,
    };
    let pointer = match crate::wirkd::client::locate(Path::new(&triple.estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk finding raise: {err}");
            return ExitCode::from(2);
        }
    };
    let payload = FindingRaisePayload {
        triple,
        kind,
        scope,
        claim,
        evidence: flag_values(rest, "--evidence"),
        contradicts: flag_values(rest, "--contradicts"),
        applies_to: flag_values(rest, "--applies-to"),
        supersedes: flag_value(rest, "--supersedes"),
        proposed_change: flag_value(rest, "--proposed-change"),
        obligation: flag_value(rest, "--obligation"),
        confirmed_by: flag_value(rest, "--confirmed-by"),
    };
    match crate::wirkd::client::call(&pointer.socket, &Request::finding_raise(payload)) {
        Ok(Reply::Ok { result, .. }) => {
            print_result(json, &result, |result| {
                println!(
                    "finding {} kind {} scope {}",
                    result["id"].as_str().unwrap_or("?"),
                    result["kind"].as_str().unwrap_or("?"),
                    result["scope"].as_str().unwrap_or("?")
                );
            });
            ExitCode::SUCCESS
        }
        Ok(Reply::Err { error, .. }) => {
            println!("Refused: {} {}", error.code, error.message);
            ExitCode::from(3)
        }
        Err(err) => {
            eprintln!("wirk finding raise: {err}");
            ExitCode::from(2)
        }
    }
}

/// `--requesting-work <id>` or `--admin`, the same exclusive pair
/// `finding list`/`atlas findings` already carry (W-B disclosure
/// response repair): `assert` writes to the target finding's own Work,
/// so `wirkd` refuses a non-admin requester off that Work's own lineage
/// before appending anything, and scopes what the reply discloses
/// exactly as `finding list` does.
fn assert_command(rest: &[String]) -> ExitCode {
    let (Some(estate), Some(finding), Some(decision), Some(by)) = (
        flag_value(rest, "--estate"),
        flag_value(rest, "--finding"),
        flag_value(rest, "--decision"),
        flag_value(rest, "--by"),
    ) else {
        return finding_usage();
    };
    let json = is_json(rest);
    let requester = flag_value(rest, "--requesting-work").map(WorkId);
    let admin = rest.iter().any(|arg| arg == "--admin");
    if admin == requester.is_some() {
        eprintln!(
            "wirk finding assert: name exactly one of --requesting-work <id> (scoped) or --admin (unscoped)"
        );
        return ExitCode::from(2);
    }
    wirkd_client_call(
        &estate,
        &Request::finding_assert(FindingAssertPayload {
            finding,
            decision,
            by,
            reason: flag_value(rest, "--reason"),
            superseded_by: flag_value(rest, "--superseded-by"),
            requester,
            admin,
        }),
        |result| {
            print_result(json, result, |result| {
                println!(
                    "finding {} settled {}",
                    result["id"].as_str().unwrap_or("?"),
                    if result["settled"].is_null() {
                        "false"
                    } else {
                        "true"
                    }
                );
                println!(
                    "  (`assert` records an unverified operator assertion; it does not settle the finding — settlement is minted by wirkd when an admitted policy class's check holds.)"
                );
            });
        },
    )
}

/// `--requesting-work <id>` or `--admin`, the same exclusive pair
/// `finding list`/`atlas findings` already carry (W-B disclosure
/// response repair): naming a requester bounds only what this reply
/// discloses — the settlement evaluation itself runs identically either
/// way, since scope admission is not settlement permission.
fn settle_command(rest: &[String]) -> ExitCode {
    let (Some(estate), Some(finding)) =
        (flag_value(rest, "--estate"), flag_value(rest, "--finding"))
    else {
        return finding_usage();
    };
    let json = is_json(rest);
    let requester = flag_value(rest, "--requesting-work").map(WorkId);
    let admin = rest.iter().any(|arg| arg == "--admin");
    if admin == requester.is_some() {
        eprintln!(
            "wirk finding settle: name exactly one of --requesting-work <id> (scoped) or --admin (unscoped)"
        );
        return ExitCode::from(2);
    }
    wirkd_client_call(
        &estate,
        &Request::finding_settle(FindingSettlePayload {
            finding,
            requester,
            admin,
        }),
        |result| {
            print_result(json, result, |result| {
                if result["settled"].is_null() {
                    println!(
                        "pending: {}",
                        result["pending"]["reason"].as_str().unwrap_or("?")
                    );
                } else {
                    println!(
                        "settled: class {}",
                        result["settled"]["authority"]["policy"]["class"]
                            .as_str()
                            .unwrap_or("?")
                    );
                }
            });
        },
    )
}

/// This verb no longer reads `--estate`: like `raise`, it now reads the
/// caller's own injected pane triple from env
/// and `wirkd` checks its currency (`TripleMismatch`/`WorkTerminal`/
/// current-run) before recording *any* attribution
/// (W-B-CORRECT.md defect 3: a `--by` string alone was previously
/// sufficient to write a durable `FindingApplied` from any shell with no
/// Work, Run, or World at all — the authority review's own executed
/// counterexample).
///
/// W-B Application repair: the caller's triple is now held to ruling
/// 0095's own currency rule — it must be the Work's *current producing
/// action*, not merely the latest attempt for its Waypoint. The Claim
/// path is therefore named separately rather than being "my own
/// triple, reinterpreted": `--claim-run <run-id>` (with `--claim-work
/// <id>` when that Run belongs to another Work on this one's lineage,
/// defaulting to the caller's own Work) cites a historical Validated
/// Done Claim as causal evidence. That cited Run is allowed to be
/// spent and its Work completed — that is what history is — while the
/// caller making the judgement stays a real, current, admitted action.
/// The old valueless `--claim` is gone: it could only ever have meant
/// the caller's own Run, which by this rule is open and has no Claim.
fn applied_command(rest: &[String]) -> ExitCode {
    let (Some(finding), Some(source), Some(revision), Some(by)) = (
        flag_value(rest, "--finding"),
        flag_value(rest, "--source"),
        flag_value(rest, "--revision"),
        flag_value(rest, "--by"),
    ) else {
        return finding_usage();
    };
    // W-B Application currentness correction, F-2. `--claim` was a real,
    // meaningful flag on this verb before the repair split the caller
    // from the Claim it cites, and this CLI's convention is to ignore an
    // argument it does not recognise. Those two facts together made an
    // explicit `--claim` request *succeed*, exit 0, and record the
    // strictly weaker `asserted` attribution — where the same command
    // against the previous build was refused. A caller asking for
    // checked Claim attribution must never be silently given an
    // unverified assertion instead, so this one retired spelling is
    // named and refused with the migration it needs. Deliberately not a
    // general unknown-flag rejector: `--totally-bogus-flag` is still
    // tolerated here exactly as it is everywhere else in this binary,
    // and changing that is a separate decision about a separate surface.
    if rest.iter().any(|arg| arg == "--claim") {
        eprintln!(
            "wirk finding applied: --claim was retired and no longer requests Claim attribution. \
             Cite the historical Validated Done Claim by its own Run instead: \
             --claim-run <run-id> [--claim-work <id>] (--claim-work defaults to your own Work). \
             Refusing rather than recording the weaker unverified `asserted` attribution this \
             flag would now silently fall back to."
        );
        return ExitCode::from(1);
    }
    let claim_run = flag_value(rest, "--claim-run");
    let claim_work = flag_value(rest, "--claim-work");
    if claim_work.is_some() && claim_run.is_none() {
        eprintln!(
            "wirk finding applied: --claim-work names a Work for --claim-run, which is missing"
        );
        return finding_usage();
    }
    let json = is_json(rest);
    let triple = match triple_from_env("applied") {
        Ok(triple) => triple,
        Err(code) => return code,
    };
    let pointer = match crate::wirkd::client::locate(Path::new(&triple.estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk finding applied: {err}");
            return ExitCode::from(2);
        }
    };
    match crate::wirkd::client::call(
        &pointer.socket,
        &Request::finding_applied(FindingAppliedPayload {
            triple,
            finding,
            source,
            revision,
            by,
            claim_run,
            claim_work,
        }),
    ) {
        Ok(Reply::Ok { result, .. }) => {
            print_result(json, &result, |result| {
                let Some(applied) = result["applied"].as_array().and_then(|list| list.last())
                else {
                    println!("applied, but no Application record was returned");
                    return;
                };
                println!(
                    "{} in {} changed from {} to {} (revision {})",
                    applied["before"]["object_id"]
                        .as_str()
                        .unwrap_or("(absent)"),
                    applied["source"].as_str().unwrap_or("?"),
                    applied["before"]["object_id"]
                        .as_str()
                        .unwrap_or("(absent)"),
                    applied["after"]["object_id"].as_str().unwrap_or("(absent)"),
                    applied["revision"].as_str().unwrap_or("?")
                );
            });
            ExitCode::SUCCESS
        }
        Ok(Reply::Err { error, .. }) => {
            println!("Refused: {} {}", error.code, error.message);
            ExitCode::from(3)
        }
        Err(err) => {
            eprintln!("wirk finding applied: {err}");
            ExitCode::from(2)
        }
    }
}

/// §3/W-B-CORRECT.md defect 2: by default this only ever sees the named
/// `--requesting-work`'s own lineage (itself, its ancestors, its
/// descendants) — naming a `--work` narrows *within* that lineage, it
/// never widens past it. `--admin` is the one, explicit, separately
/// named escape hatch to the old unscoped estate-wide view; it is a
/// distinct operator decision, never the default.
fn list_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return finding_usage();
    };
    let json = is_json(rest);
    let work = flag_value(rest, "--work").map(WorkId);
    let requester = flag_value(rest, "--requesting-work").map(WorkId);
    let admin = rest.iter().any(|arg| arg == "--admin");
    if !admin && requester.is_none() {
        eprintln!("wirk finding list: --requesting-work <id> is required unless --admin is given");
        return finding_usage();
    }
    wirkd_client_call(
        &estate,
        &Request::finding_list(FindingListPayload {
            work,
            requester,
            admin,
        }),
        |result| {
            print_result(json, result, |result| {
                let count = result["findings"].as_array().map(|a| a.len()).unwrap_or(0);
                println!("{count} finding(s)");
            });
        },
    )
}
