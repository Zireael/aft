//! Quote selection for ES module specifiers.

use std::io::Read;
use std::path::Path;

use crate::parser::{parse_source_with_cached_parser, LangId};

/// Count only module-source strings, not quotes in comments, bindings or attributes.
pub(crate) fn statement_quote(source: &str) -> Option<char> {
    let tree =
        parse_source_with_cached_parser(Path::new("quotes.ts"), source, LangId::TypeScript).ok()?;
    module_quotes(tree.root_node(), source).into_iter().next()
}

fn module_quotes(node: tree_sitter::Node<'_>, source: &str) -> Vec<char> {
    let mut quotes = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if matches!(child.kind(), "import_statement" | "export_statement") {
            if let Some(module) = child.child_by_field_name("source") {
                if let Some(quote @ ('\'' | '"')) = source[module.byte_range()].chars().next() {
                    quotes.push(quote);
                }
            }
        }
    }
    quotes
}

pub(crate) fn preferred_quote(
    source: &str,
    tree: &tree_sitter::Tree,
    lang: LangId,
    file: &Path,
    root: Option<&Path>,
) -> char {
    let quotes = if lang == LangId::Vue {
        super::vue_script_content_range(tree)
            .and_then(|(start, end)| {
                let script = &source[start..end];
                parse_source_with_cached_parser(file, script, LangId::TypeScript)
                    .ok()
                    .map(|tree| module_quotes(tree.root_node(), script))
            })
            .unwrap_or_default()
    } else {
        module_quotes(tree.root_node(), source)
    };
    let singles = quotes.iter().filter(|&&q| q == '\'').count();
    let doubles = quotes.len() - singles;
    match singles.cmp(&doubles) {
        std::cmp::Ordering::Greater => '\'',
        std::cmp::Ordering::Less => '"',
        std::cmp::Ordering::Equal => quotes
            .first()
            .copied()
            .unwrap_or_else(|| config_quote(file, root).unwrap_or('"')),
    }
}

fn config_quote(file: &Path, root: Option<&Path>) -> Option<char> {
    let root = root?;
    let mut dir = file.parent()?;
    if !dir.starts_with(root) {
        return None;
    }
    let mut reads = 0;
    loop {
        for name in [
            "biome.json",
            "biome.jsonc",
            ".prettierrc",
            ".prettierrc.json",
            ".prettierrc.json5",
            ".prettierrc.yaml",
            ".prettierrc.yml",
            ".prettierrc.js",
            ".prettierrc.cjs",
            ".prettierrc.mjs",
            ".prettierrc.toml",
            "prettier.config.js",
            "prettier.config.cjs",
            "prettier.config.mjs",
            "package.json",
        ] {
            let Ok(mut config) = std::fs::File::open(dir.join(name)) else {
                continue;
            };
            reads += 1;
            if reads > 4 {
                return None;
            }
            let mut text = String::new();
            if config
                .by_ref()
                .take(65_537)
                .read_to_string(&mut text)
                .is_err()
                || text.len() > 65_536
            {
                continue;
            }
            let json =
                serde_json::from_str::<serde_json::Value>(&crate::jsonc::strip_jsonc(&text)).ok();
            if name.starts_with("biome") {
                if let Some(style) = json
                    .as_ref()
                    .and_then(|v| v.pointer("/javascript/formatter/quoteStyle"))
                    .and_then(|v| v.as_str())
                {
                    match style {
                        "single" => return Some('\''),
                        "double" => return Some('"'),
                        _ => {}
                    }
                }
            } else {
                let value = if name == "package.json" {
                    json.as_ref().and_then(|v| v.get("prettier"))
                } else {
                    json.as_ref()
                };
                if let Some(single) = value
                    .and_then(|v| v.get("singleQuote"))
                    .and_then(|v| v.as_bool())
                {
                    return Some(if single { '\'' } else { '"' });
                }
                // Static YAML/JS/TOML settings are readable without executing project code.
                if name != "package.json" && json.is_none() {
                    let setting =
                        regex::Regex::new(r#"\bsingleQuote[\"']?\s*[:=]\s*(true|false)\b"#).ok()?;
                    if let Some(captures) = setting.captures(&text) {
                        return Some(if &captures[1] == "true" { '\'' } else { '"' });
                    }
                }
            }
        }
        if dir == root {
            break;
        }
        dir = dir.parent()?;
    }
    None
}

pub(crate) fn module_literal(module: &str, quote: char) -> String {
    let mut literal = String::new();
    literal.push(quote);
    for ch in module.chars() {
        match ch {
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            '\u{2028}' => literal.push_str("\\u2028"),
            '\u{2029}' => literal.push_str("\\u2029"),
            ch if ch == quote => {
                literal.push('\\');
                literal.push(ch);
            }
            ch if ch.is_control() => literal.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => literal.push(ch),
        }
    }
    literal.push(quote);
    literal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str, root: &Path, extension: &str) -> String {
        let file = root.join(format!("file.{extension}"));
        let lang = if extension == "vue" {
            LangId::Vue
        } else {
            LangId::TypeScript
        };
        let tree = parse_source_with_cached_parser(&file, source, lang).unwrap();
        let quote = preferred_quote(source, &tree, lang, &file, Some(root));
        crate::imports::generate_import_line_with_namespace_and_attribute_clause_and_quote(
            lang,
            "../../shared/logger",
            &["log".into()],
            None,
            None,
            false,
            None,
            Some(quote),
        )
    }

    #[test]
    fn double_quoted_add() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            render("import { a } from \"a\";", dir.path(), "ts"),
            "import { log } from \"../../shared/logger\";"
        );
    }

    #[test]
    fn single_quoted_add() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            render("import { a } from 'a';", dir.path(), "ts"),
            "import { log } from '../../shared/logger';"
        );
    }

    #[test]
    fn majority_and_first_statement_tie() {
        let dir = tempfile::tempdir().unwrap();
        for source in [
            "import { a } from 'a'; import { b } from \"b\"; export { c } from \"c\";",
            "export { a } from \"a\"; import { b } from 'b';",
            "// import 'ignored';\nimport { a } from \"a\" with { type: 'json' };",
        ] {
            assert_eq!(
                render(source, dir.path(), "ts"),
                "import { log } from \"../../shared/logger\";"
            );
        }
        assert_eq!(
            render(
                "<script setup lang=\"ts\">import { a } from 'a';</script>",
                dir.path(),
                "vue"
            ),
            "import { log } from '../../shared/logger';"
        );
    }

    #[test]
    fn formatter_config_and_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            render("const a = 'ignored';", dir.path(), "ts"),
            "import { log } from \"../../shared/logger\";"
        );
        for (name, content) in [
            (
                "biome.json",
                r#"{"javascript":{"formatter":{"quoteStyle":"single"}}}"#,
            ),
            (
                "biome.jsonc",
                "{ // comment\n\"javascript\": {\"formatter\": {\"quoteStyle\": \"single\",}},}",
            ),
            (".prettierrc", r#"{"singleQuote":true}"#),
            ("package.json", r#"{"prettier":{"singleQuote":true}}"#),
            (".prettierrc.yaml", "singleQuote: true\n"),
            (".prettierrc.cjs", "module.exports = { singleQuote: true };"),
        ] {
            std::fs::write(dir.path().join(name), content).unwrap();
            assert_eq!(
                render("const a = 1;", dir.path(), "ts"),
                "import { log } from '../../shared/logger';",
                "{name}"
            );
            std::fs::remove_file(dir.path().join(name)).unwrap();
        }
    }

    #[test]
    fn config_is_nearest_and_stops_at_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join(".prettierrc"), r#"{"singleQuote":true}"#).unwrap();
        assert_eq!(config_quote(&nested.join("file.ts"), Some(&root)), None);
        std::fs::write(root.join(".prettierrc"), r#"{"singleQuote":true}"#).unwrap();
        assert_eq!(
            config_quote(&nested.join("file.ts"), Some(&root)),
            Some('\'')
        );
        std::fs::write(
            nested.join("biome.json"),
            r#"{"javascript":{"formatter":{"quoteStyle":"double"}}}"#,
        )
        .unwrap();
        assert_eq!(
            config_quote(&nested.join("file.ts"), Some(&root)),
            Some('"')
        );
    }

    #[test]
    fn module_path_escaping() {
        assert_eq!(module_literal("a'\"\\b\n", '\''), "'a\\'\"\\\\b\\n'");
        assert_eq!(module_literal("a'\"\\b\n", '"'), "\"a'\\\"\\\\b\\n\"");
    }
}
