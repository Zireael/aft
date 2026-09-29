//! Trigram plane of a per-checkout view: the family segment, the generation
//! overlay and the live delta, with one active source per relative path.
//!
//! Implements `contracts::PlaneAdapter` for `FamilyPlane::Trigram`. Built on
//! `segment_store` payloads and segments and on `snapshot::LiveDelta`.
