use crate::admission::{QueryScope, admit};
use crate::domain::now_unix_millis;
use crate::{
    AtlasError, AtlasStore, ExactCoordinate, Relationship, RelationshipId, RelationshipKind,
    ResolveOutcome,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelationshipError {
    #[error(transparent)]
    Atlas(#[from] AtlasError),
    #[error("endpoint or evidence coordinate is not admissible under this scope")]
    Inadmissible,
    #[error("endpoint or evidence coordinate does not resolve: {0}")]
    Unresolved(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RelationshipView {
    Disclosed(Box<Relationship>),
    Filtered,
}

/// Admits a `GovernedBy` relationship only if every endpoint and evidence
/// coordinate is both admissible under `scope` and independently resolves;
/// a lexical mention proves nothing. Retrying an already-admitted
/// relationship is idempotent (same `RelationshipId`), never a duplicate.
///
/// Ruling 0077: admission is gated on read admissibility, not on
/// `AdmittedSource.access`. Recording a durable, evidenced assertion in
/// Atlas state is distinct from source mutation authority; requiring
/// `Access::Write` on both endpoints would wrongly demand mutation rights
/// over a governing Read knowledge source and defeat the cross-source
/// scenario this crate exists to serve. `access` remains real, useful
/// per-source permission data for a caller's own decisions — it is not
/// read by this function, and that is deliberate, not an oversight. This
/// is W2's trusted-library scope: the caller (today, tests; W3, `wirkd`)
/// is trusted to have already resolved the canonical estate, the real
/// journaled Work, and a real producer identity before calling. A
/// caller-supplied `producer` string here is not authenticated authority;
/// W3 must bind it to an admitted coordinator identity at the public
/// boundary.
#[allow(clippy::too_many_arguments)]
pub fn admit_relationship(
    store: &mut AtlasStore,
    scope: &QueryScope,
    requested_source: Option<&str>,
    kind: RelationshipKind,
    from: ExactCoordinate,
    to: ExactCoordinate,
    evidence: Vec<ExactCoordinate>,
    producer: &str,
) -> Result<Relationship, RelationshipError> {
    let (admitted, _) = admit(store.memberships(), scope, requested_source);
    let mut coordinates: Vec<&ExactCoordinate> = vec![&from, &to];
    coordinates.extend(evidence.iter());
    for coordinate in coordinates {
        let Some(source) = admitted
            .iter()
            .find(|source| source.membership.id == coordinate.membership)
        else {
            return Err(RelationshipError::Inadmissible);
        };
        match store.resolve_exact(&source.membership, coordinate)? {
            ResolveOutcome::Resolved(_) => {}
            other => return Err(RelationshipError::Unresolved(format!("{other:?}"))),
        }
    }
    let id = RelationshipId::compute(kind, &from, &to, &evidence, producer);
    if let Some(existing) = store.relationships()?.into_iter().find(|r| r.id == id) {
        return Ok(existing);
    }
    let relationship = Relationship {
        id,
        kind,
        from,
        to,
        evidence,
        producer: producer.to_owned(),
        published_at_unix_millis: now_unix_millis(),
    };
    store.append_relationship(&relationship)?;
    Ok(relationship)
}

/// Returns every stored relationship touching `coordinate` as `from` or
/// `to`, disclosed only if both endpoints and all evidence are admissible
/// under `scope`; otherwise an opaque `Filtered` marker that names nothing
/// about the hidden side.
pub fn relationships_for(
    store: &AtlasStore,
    scope: &QueryScope,
    requested_source: Option<&str>,
    coordinate: &ExactCoordinate,
) -> Result<Vec<RelationshipView>, AtlasError> {
    let (admitted, _) = admit(store.memberships(), scope, requested_source);
    let admitted_ids: std::collections::BTreeSet<_> = admitted
        .iter()
        .map(|source| source.membership.id.clone())
        .collect();
    let mut views = Vec::new();
    for relationship in store.relationships()? {
        if relationship.from != *coordinate && relationship.to != *coordinate {
            continue;
        }
        let endpoints_ok = admitted_ids.contains(&relationship.from.membership)
            && admitted_ids.contains(&relationship.to.membership);
        let evidence_ok = relationship
            .evidence
            .iter()
            .all(|coordinate| admitted_ids.contains(&coordinate.membership));
        if endpoints_ok && evidence_ok {
            views.push(RelationshipView::Disclosed(Box::new(relationship)));
        } else {
            views.push(RelationshipView::Filtered);
        }
    }
    Ok(views)
}
