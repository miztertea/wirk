use crate::admission::{QueryScope, admit};
use crate::domain::now_unix_millis;
use crate::{
    AtlasError, AtlasStore, ExactCoordinate, GenerationId, MembershipId, Relationship,
    RelationshipId, RelationshipKind, ResolveOutcome,
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
    let admitted_ids = admitted_memberships(store, scope, requested_source);
    Ok(store
        .relationships()?
        .into_iter()
        .filter(|relationship| relationship.from == *coordinate || relationship.to == *coordinate)
        .map(|relationship| disclose(&admitted_ids, relationship))
        .collect())
}

/// One resource, addressed the way a delivered coordinate addresses it:
/// a membership, the generation that membership was read at, and the
/// path. Deliberately not a whole `ExactCoordinate` — a bound item's
/// byte and line span is the span the assembler chose to deliver, and an
/// edge is about the resource, not about that window.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResourceKey {
    pub membership: MembershipId,
    pub generation: GenerationId,
    pub path: Vec<u8>,
}

impl ResourceKey {
    fn matches(&self, coordinate: &ExactCoordinate) -> bool {
        self.names_resource(coordinate) && coordinate.generation == self.generation
    }

    /// The same resource — this source, this path — at whatever edition
    /// the coordinate was recorded against.
    ///
    /// This is deliberately *not* what an edge is followed on. It is
    /// what lets a caller tell "this estate admitted nothing about this
    /// resource" apart from "this estate admitted something about this
    /// resource, at an edition you did not capture" without following a
    /// single one of them (ruling 0128 F1).
    fn names_resource(&self, coordinate: &ExactCoordinate) -> bool {
        coordinate.membership == self.membership && coordinate.path == self.path
    }
}

/// What one pass of the relationship log found for a whole frontier.
///
/// Two separate facts, because they are read for two different reasons.
/// `views` is what may be followed: edges admitted against exactly the
/// generations the caller captured, each already through the disclosure
/// gate. `admitted_at_another_edition` is what may not be followed and
/// must still not vanish: disclosable edges whose `from` end names one
/// of `resources` by source and path at a *different* generation.
///
/// The count is only ever of edges this caller could have been shown
/// whole. An edge whose far end or evidence lies outside admission is
/// not counted here at all — saying "there is one you may not see, at an
/// edition you did not capture" about a resource the caller cannot see
/// either would be a leak dressed as honesty, and the whole point of the
/// existing `Filtered` marker is that a hidden side is described by
/// nothing.
pub struct FrontierRelationships {
    pub views: Vec<RelationshipView>,
    pub admitted_at_another_edition: usize,
}

/// Every stored relationship whose `from` end names one of `resources`,
/// under exactly the disclosure gate `relationships_for` applies —
/// **one** pass over the relationship log for the whole set, rather than
/// one pass per coordinate.
///
/// The batching is the point: a stage projection follows governance out
/// of every bound item it holds, and re-reading and re-admitting the
/// whole relationship log once per item is the shape that does not
/// survive a real estate. Admission is computed once here and applied to
/// every candidate, so what a caller can see is identical to what
/// `relationships_for` would have shown it, edge for edge.
///
/// The generation is part of the match. An edge admitted against a
/// superseded generation is evidence about bytes this caller is not
/// pinned to, and silently treating it as current would be exactly the
/// substituted provenance ruling 0126 refuses. Callers that want such an
/// edge must re-admit it against the generation they actually read.
pub fn relationships_from_resources(
    store: &AtlasStore,
    scope: &QueryScope,
    requested_source: Option<&str>,
    resources: &[ResourceKey],
) -> Result<FrontierRelationships, AtlasError> {
    let mut found = FrontierRelationships {
        views: Vec::new(),
        admitted_at_another_edition: 0,
    };
    if resources.is_empty() {
        return Ok(found);
    }
    let admitted_ids = admitted_memberships(store, scope, requested_source);
    for relationship in store.relationships()? {
        if resources
            .iter()
            .any(|resource| resource.matches(&relationship.from))
        {
            found.views.push(disclose(&admitted_ids, relationship));
            continue;
        }
        if resources
            .iter()
            .any(|resource| resource.names_resource(&relationship.from))
            && matches!(
                disclose(&admitted_ids, relationship),
                RelationshipView::Disclosed(_)
            )
        {
            found.admitted_at_another_edition += 1;
        }
    }
    Ok(found)
}

fn admitted_memberships(
    store: &AtlasStore,
    scope: &QueryScope,
    requested_source: Option<&str>,
) -> std::collections::BTreeSet<MembershipId> {
    let (admitted, _) = admit(store.memberships(), scope, requested_source);
    admitted
        .iter()
        .map(|source| source.membership.id.clone())
        .collect()
}

/// The one disclosure decision, shared by both selectors: an edge is
/// disclosed only when both endpoints *and* every evidence coordinate
/// are admissible, and is otherwise an opaque marker naming nothing
/// about the hidden side.
fn disclose(
    admitted_ids: &std::collections::BTreeSet<MembershipId>,
    relationship: Relationship,
) -> RelationshipView {
    let endpoints_ok = admitted_ids.contains(&relationship.from.membership)
        && admitted_ids.contains(&relationship.to.membership);
    let evidence_ok = relationship
        .evidence
        .iter()
        .all(|coordinate| admitted_ids.contains(&coordinate.membership));
    if endpoints_ok && evidence_ok {
        RelationshipView::Disclosed(Box::new(relationship))
    } else {
        RelationshipView::Filtered
    }
}
