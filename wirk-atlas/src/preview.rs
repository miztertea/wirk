//! `atlas acquire --dry-run`'s own answer shape: what a real acquisition
//! of `repository` at `revision` would find, classified from names,
//! sizes and (only for a document tree, whose own walk already reads
//! this much — see `doctree::preview`'s doc) a bounded content read —
//! never from running extraction, chunking, embedding, or writing a
//! generation. `AtlasStore::preview` is the only place these buckets
//! are built; `git::preview`/`doctree::preview` each fill one.
//!
//! Four buckets, not the five `CoverageDisposition` names: `indexed`
//! and `error` are both real-acquisition-only outcomes of actually
//! trying to extract a candidate, which this preview does not do, so
//! both real outcomes fold into `candidate` here. `unclassified` has no
//! `CoverageDisposition` counterpart at all — it is the honest "the
//! name alone does not decide it" answer for an input whose real
//! disposition depends on a content read this preview may not perform
//! (see `content_sniffed`).

use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PreviewBucket {
    pub count: u64,
    pub bytes: u64,
}

impl PreviewBucket {
    pub(crate) fn add(&mut self, bytes: u64) {
        self.count += 1;
        self.bytes += bytes;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewReport {
    /// `"git"` or `"document-tree"` — the same spelling `--kind` takes.
    pub kind: String,
    pub repository: String,
    /// The resolved commit for `"git"`; the document-tree sentinel
    /// (`DOCUMENT_TREE_CURRENT_OBSERVATION`) otherwise.
    pub revision: String,
    /// Extension/family-recognized: a real acquisition would read and
    /// attempt to extract this input. Whether that attempt actually
    /// succeeds is not decided here.
    pub candidate: PreviewBucket,
    /// Refused by the estate's own fixed secret-like policy, before any
    /// read — identical to a real acquisition's `Excluded`.
    pub excluded: PreviewBucket,
    /// No extractor recognizes this input's family, or it exceeds the
    /// estate's configured per-file bound — identical to a real
    /// acquisition's `Unsupported`.
    pub unsupported: PreviewBucket,
    /// The name alone does not decide it, and this preview did not read
    /// its content to find out (`content_sniffed` says whether it could
    /// have). A real acquisition of the same input lands in one of the
    /// other four dispositions, never left unclassified.
    pub unclassified: PreviewBucket,
    /// Listed, but its bytes could not be read (permission denied,
    /// vanished mid-walk). Document-tree only — Git's preview reads no
    /// blob, so it never observes this.
    pub unavailable: PreviewBucket,
    pub total: PreviewBucket,
    /// Whether reaching `unclassified` above still cost a bounded
    /// content read. `false` for `"git"`, whose preview reads
    /// `git ls-tree`'s own metadata only and no blob; `true` for
    /// `"document-tree"`, whose walk already performs the same bounded
    /// sniff/read a real acquisition's own capture would.
    pub content_sniffed: bool,
}

impl PreviewReport {
    pub(crate) fn new(
        kind: &str,
        repository: String,
        revision: String,
        content_sniffed: bool,
    ) -> Self {
        Self {
            kind: kind.to_string(),
            repository,
            revision,
            candidate: PreviewBucket::default(),
            excluded: PreviewBucket::default(),
            unsupported: PreviewBucket::default(),
            unclassified: PreviewBucket::default(),
            unavailable: PreviewBucket::default(),
            total: PreviewBucket::default(),
            content_sniffed,
        }
    }

    /// Folds the five typed buckets into `total`. Called once, after
    /// every entry has been classified.
    pub(crate) fn finish(mut self) -> Self {
        for bucket in [
            self.candidate,
            self.excluded,
            self.unsupported,
            self.unclassified,
            self.unavailable,
        ] {
            self.total.count += bucket.count;
            self.total.bytes += bucket.bytes;
        }
        self
    }
}
