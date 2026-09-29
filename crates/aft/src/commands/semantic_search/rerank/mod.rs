//! Cross-encoder reranking of the head of the canonical result list.
//!
//! A reranker scores the prose `query` against a short text built for each of
//! the first entries of the canonical list and reorders only those entries.
//! Backends implement [`RerankBackend`]; everything that decides *what* is
//! reranked (how many entries, which tiers, the candidate text) lives in this
//! module so it stays inside the search-quality ranking fence.

// The backends and the ranking hook are wired in follow-up changes; until then
// the trait and its test backend are only exercised by tests.
#![allow(dead_code)]

use std::time::Instant;

/// One candidate document handed to a backend: the bounded candidate text.
pub(crate) struct RerankDoc<'a> {
    pub(crate) text: &'a str,
}

/// Names the exact scoring function a backend applies. Two backends with equal
/// fingerprints must produce equal scores for equal inputs, so the fingerprint
/// is part of every memoized ordering and every recorded fixture score.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RerankFingerprint {
    pub(crate) backend: &'static str,
    pub(crate) model: String,
    pub(crate) revision: String,
}

/// Why a backend returned no scores. None of these is a search error: the
/// caller keeps the fused order and reports one short note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RerankError {
    /// The backend cannot serve now (model not provisioned yet, runtime
    /// missing, worker busy, process shutting down).
    Unavailable(String),
    /// No answer arrived before the caller's deadline.
    Timeout,
    /// The backend rejected the request (bad input, over a size limit).
    Refused(String),
    /// Scoring started and failed.
    Failed(String),
}

pub(crate) trait RerankBackend: Send + Sync {
    fn fingerprint(&self) -> RerankFingerprint;
    fn max_batch(&self) -> usize;
    /// One score per doc, same order; must return by `deadline` or Err(Timeout).
    fn score(
        &self,
        query: &str,
        docs: &[RerankDoc<'_>],
        deadline: Instant,
    ) -> Result<Vec<f32>, RerankError>;
}

/// A backend that scores every document zero, so a stable reorder keeps the
/// prior order. Tests use it where a backend must exist but must not move
/// anything.
#[cfg(test)]
pub(crate) struct NoopRerankBackend;

#[cfg(test)]
impl RerankBackend for NoopRerankBackend {
    fn fingerprint(&self) -> RerankFingerprint {
        RerankFingerprint {
            backend: "noop",
            model: "noop".to_string(),
            revision: "0".to_string(),
        }
    }

    fn max_batch(&self) -> usize {
        usize::MAX
    }

    fn score(
        &self,
        _query: &str,
        docs: &[RerankDoc<'_>],
        _deadline: Instant,
    ) -> Result<Vec<f32>, RerankError> {
        Ok(vec![0.0; docs.len()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn noop_backend_returns_one_score_per_doc() {
        let backend = NoopRerankBackend;
        let docs = [RerankDoc { text: "a" }, RerankDoc { text: "b" }];
        let scores = backend
            .score("q", &docs, Instant::now() + Duration::from_secs(1))
            .expect("noop backend never fails");
        assert_eq!(scores, vec![0.0, 0.0]);
        assert_eq!(backend.fingerprint().backend, "noop");
    }
}
