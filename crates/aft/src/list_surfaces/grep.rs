//! Grep list surface truncation adapter.
//!
//! Produces the truncation envelope for grep match lists (`payload.matches`),
//! accounting for directory walk boundaries (such as search timeouts or skipped
//! filesystem mounts) and match caps (such as the page size, `offset` paging,
//! or the output byte budget).

use crate::list_envelope::{ListEnvelope, Reason, Total, Unit};
use serde_json::Value;

pub const COMMAND: &str = "grep";
pub const LIST_ID: &str = "payload.matches";
pub const UNIT: Unit = Unit::Rows;
pub const NARROW: &[&str] = &["offset", "path", "include", "exclude"];

/// Build the list truncation envelope for a grep response from its component parts.
///
/// Returns `None` if no truncation cause fired (the response is complete).
pub fn build_grep_envelope_from_parts(
    shown: usize,
    total_matches: usize,
    matches_count: usize,
    truncated: bool,
    walk_truncated: bool,
    skipped_foreign_mounts: usize,
) -> Option<ListEnvelope> {
    build_grep_envelope_for_page(
        0,
        shown,
        total_matches,
        matches_count,
        truncated,
        walk_truncated,
        skipped_foreign_mounts,
    )
}

/// Build the list truncation envelope for one page of grep matches.
///
/// `offset` is how many matching lines earlier pages covered, `matches_count`
/// is how many matches this page held and `shown` how many of them were
/// printed (the output byte budget can stop rendering early). A page that
/// skipped earlier rows, left rows unprinted, or has rows after it is not the
/// complete list, so it gets a `cap` envelope.
pub fn build_grep_envelope_for_page(
    offset: usize,
    shown: usize,
    total_matches: usize,
    matches_count: usize,
    truncated: bool,
    walk_truncated: bool,
    skipped_foreign_mounts: usize,
) -> Option<ListEnvelope> {
    let walk_cause = walk_truncated || skipped_foreign_mounts > 0;
    let seen = offset.saturating_add(matches_count);
    let cap_cause = truncated || shown < matches_count || offset > 0 || total_matches > seen;

    if !walk_cause && !cap_cause {
        return None;
    }

    let mut causes = Vec::new();
    if walk_cause {
        causes.push(Reason::Walk);
    }
    if cap_cause {
        causes.push(Reason::Cap);
    }

    let total = if walk_cause {
        // Any traversal cut means the true total is unknown, making the count a lower bound.
        Total::AtLeast(total_matches.max(offset.saturating_add(shown)))
    } else if truncated {
        // When the executor hits its result cap, the match count is a lower bound.
        Total::AtLeast(total_matches.max(seen))
    } else {
        // When enumeration completed and only paging or the output budget cut
        // the printed rows, the total count is exact.
        Total::Exact(total_matches.max(seen))
    };

    Some(ListEnvelope::new(shown, total, UNIT, causes, NARROW))
}

/// Build the list truncation envelope for a grep response JSON payload.
///
/// Inspects the response payload for walk boundaries and match caps, deriving
/// the rendered match count from the formatter seam. Returns `None` if the
/// result is complete.
pub fn build_grep_envelope(data: &Value) -> Option<ListEnvelope> {
    let matches = data.get("matches").and_then(Value::as_array);
    let matches_count = matches.map(|m| m.len()).unwrap_or(0);
    let total_matches = data
        .get("total_matches")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(matches_count);
    let offset = data
        .get("offset")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);
    let truncated = data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let walk_truncated = data
        .get("walk_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let skipped_foreign_mounts = data
        .get("skipped_foreign_mounts")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);

    let shown = crate::subc_format::rendered_grep_match_count(data);

    build_grep_envelope_for_page(
        offset,
        shown,
        total_matches,
        matches_count,
        truncated,
        walk_truncated,
        skipped_foreign_mounts,
    )
}
