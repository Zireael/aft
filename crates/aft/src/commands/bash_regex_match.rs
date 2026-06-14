use crate::bash_background::watches::WatchPattern;
use crate::protocol::{RawRequest, Response};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct BashRegexMatchParams {
    pattern: String,
    text: String,
}

#[derive(Debug, PartialEq, Eq)]
struct BashRegexMatchResult {
    matched: bool,
    match_text: Option<String>,
    match_offset: Option<u64>,
    match_index_chars: Option<usize>,
}

fn regex_match(pattern: &str, text: &str) -> Result<BashRegexMatchResult, regex::Error> {
    let pattern = WatchPattern::regex(pattern)?;
    let WatchPattern::Regex(regex) = pattern else {
        unreachable!("WatchPattern::regex always returns the Regex variant");
    };

    let Some(found) = regex.find(text) else {
        return Ok(BashRegexMatchResult {
            matched: false,
            match_text: None,
            match_offset: None,
            match_index_chars: None,
        });
    };

    let start = found.start();
    Ok(BashRegexMatchResult {
        matched: true,
        match_text: Some(found.as_str().to_string()),
        match_offset: Some(start as u64),
        match_index_chars: Some(text[..start].chars().count()),
    })
}

pub fn handle(req: &RawRequest) -> Response {
    let raw_params = req
        .params
        .get("params")
        .cloned()
        .unwrap_or_else(|| req.params.clone());
    let params = match serde_json::from_value::<BashRegexMatchParams>(raw_params) {
        Ok(params) => params,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("bash_regex_match: invalid params: {e}"),
            );
        }
    };

    match regex_match(&params.pattern, &params.text) {
        Ok(result) if result.matched => Response::success(
            &req.id,
            json!({
                "matched": true,
                "match_text": result.match_text,
                "match_offset": result.match_offset,
                "match_index_chars": result.match_index_chars,
            }),
        ),
        Ok(_) => Response::success(&req.id, json!({ "matched": false })),
        Err(e) => Response::error(
            &req.id,
            "invalid_regex",
            format!("bash_regex_match: invalid regex: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn catastrophic_backtracking_pattern_returns_quickly_without_match() {
        let text = "a".repeat(65_536);
        let started = Instant::now();

        let result = regex_match("(a+)+b", &text).expect("regex should compile");

        assert!(!result.matched);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "Rust regex matching should be linear for catastrophic JS patterns"
        );
    }

    #[test]
    fn normal_match_reports_byte_offset_and_char_index() {
        let result = regex_match("ready", "αβ ready").expect("regex should compile");

        assert_eq!(
            result,
            BashRegexMatchResult {
                matched: true,
                match_text: Some("ready".to_string()),
                match_offset: Some(5),
                match_index_chars: Some(3),
            }
        );
    }

    #[test]
    fn regex_uses_multiline_anchors_like_async_watches() {
        let result = regex_match("^foo$", "bar\nfoo\nbaz").expect("regex should compile");

        assert_eq!(
            result,
            BashRegexMatchResult {
                matched: true,
                match_text: Some("foo".to_string()),
                match_offset: Some(4),
                match_index_chars: Some(4),
            }
        );
    }
}
