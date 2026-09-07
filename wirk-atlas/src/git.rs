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
