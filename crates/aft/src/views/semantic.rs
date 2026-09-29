//! Semantic plane of a per-checkout view, reusing the shared-base overlay:
//! base vectors from the generation, local replacements and tombstones from
//! the live delta. Implements `contracts::PlaneAdapter` for
//! `FamilyPlane::Semantic`.
