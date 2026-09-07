use crate::Membership;
use wirk_core::{Access, RepositoryBinding};

/// A caller-declared permission scope. `Work` grants must be supplied
/// explicitly by the coordinator (never derived here from a journal); an
/// empty grant list admits nothing, it does not mean estate-wide. Estate
/// orientation is the one deliberate estate-wide variant, for status/
/// orientation tooling that has no Work.
#[derive(Debug, Clone)]
pub enum QueryScope {
    Work(Vec<RepositoryBinding>),
    EstateOrientation,
}

#[derive(Debug, Clone)]
pub struct AdmittedSource {
    pub membership: Membership,
    /// `None` only for `EstateOrientation`, which carries no Work access.
    ///
    /// Ruling 0077: this is real source-permission data for a caller's own
    /// use (e.g. deciding whether a Route may write through a repository),
    /// not a gate this crate itself enforces on relationship publication.
    /// `admit_relationship` (`relationship.rs`) intentionally does not read
    /// it: recording an evidenced assertion in Atlas state is distinct from
    /// mutating a source, and requiring `Write` here would wrongly demand
    /// mutation rights over a governing Read knowledge source.
    pub access: Option<Access>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmissionSummary {
    pub admitted: usize,
    pub denied: usize,
}

/// Filters catalog memberships to those the caller's scope actually
/// grants, before any ranking or blob read happens. A requested source
/// narrows this set; it never adds a membership the scope did not grant.
pub(crate) fn admit<'a>(
    memberships: impl Iterator<Item = &'a Membership>,
    scope: &QueryScope,
    requested_source: Option<&str>,
) -> (Vec<AdmittedSource>, AdmissionSummary) {
    let mut admitted = Vec::new();
    let mut denied = 0usize;
    for membership in memberships {
        if let Some(wanted) = requested_source
            && membership.alias != wanted
        {
            continue;
        }
        match scope {
            QueryScope::Work(grants) => {
                if let Some(grant) = grants.iter().find(|grant| grant.name == membership.alias) {
                    admitted.push(AdmittedSource {
                        membership: membership.clone(),
                        access: Some(grant.access),
                    });
                } else {
                    denied += 1;
                }
            }
            QueryScope::EstateOrientation => {
                admitted.push(AdmittedSource {
                    membership: membership.clone(),
                    access: None,
                });
            }
        }
    }
    let summary = AdmissionSummary {
        admitted: admitted.len(),
        denied,
    };
    (admitted, summary)
}
