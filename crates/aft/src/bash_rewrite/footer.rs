/// Add the bash-rewrite footer to a tool's human-readable output.
///
/// We intentionally keep this a single short line. The agent already wrote
/// the original bash command in their tool call so we don't repeat it; the
/// only signal we need to surface is "next time, use the dedicated tool"
/// because that's what changes their behavior on the next turn.
pub fn add_footer(response_output: &str, replacement_tool: &str) -> String {
    format!(
        "{}\n\nPrefer `{}` tool over bash.",
        response_output, replacement_tool
    )
}

/// Add the code-search redirect footer to a rewritten grep/rg output.
///
/// Unlike the generic [`add_footer`], this is an enforced "DO NOT" steer
/// because running grep/rg through bash to find code is the antipattern we
/// most want to kill: it's unindexed, unranked, and serial. When the plugin
/// registered `aft_search` for this surface (`aft_search_registered`), the
/// footer points there (it auto-routes concepts, identifiers, regex, AND
/// literals); otherwise it points at the indexed `grep` tool.
pub fn add_grep_footer(response_output: &str, aft_search_registered: bool) -> String {
    let hint = if aft_search_registered {
        "DO NOT search code by running grep/rg in bash \u{2014} it is unindexed, unranked, and serial. Use the `aft_search` tool instead (it auto-routes concepts, identifiers, regex, and literals)."
    } else {
        "DO NOT search code by running grep/rg in bash \u{2014} it is unindexed, unranked, and serial. Use the `grep` tool instead (indexed and ranked)."
    };
    format!("{response_output}\n\n{hint}")
}
