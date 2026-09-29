//! Source-independent method linking for manifest views. Extraction records only
//! syntactic receiver evidence; joining never reparses or opens checkout files.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tree_sitter::Node;

use super::{BlobRefKind, BlobSymbol, ManifestJoinError, ParseBlob};
use crate::parser::{grammar_for, LangId};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DispatchFacts {
    pub types: Vec<TypeHint>,
    pub methods: Vec<MethodHint>,
    pub sites: Vec<SiteHint>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TypeHint {
    pub name: String,
    pub bases: Vec<String>,
    pub interface: bool,
    pub closed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MethodHint {
    pub owner: String,
    pub trait_name: Option<String>,
    pub name: String,
    pub symbol: String,
    pub has_body: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SiteHint {
    pub ordinal: u32,
    pub caller: Option<String>,
    pub line: u32,
    pub member: Option<String>,
    pub receiver: Option<String>,
    pub dynamic: bool,
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or_default()
}
fn field<'a>(node: Node<'a>, names: &[&str]) -> Option<Node<'a>> {
    names.iter().find_map(|name| node.child_by_field_name(name))
}
fn descendants(node: Node<'_>) -> Vec<Node<'_>> {
    let mut result = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        result.push(node);
        stack.extend(node.named_children(&mut node.walk()));
    }
    result
}
fn type_name(raw: &str) -> String {
    raw.trim()
        .trim_start_matches(':')
        .trim()
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim_start_matches('*')
        .trim_start_matches("dyn ")
        .trim_start_matches("impl ")
        .split(['<', '[', '?'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}
fn enclosing<'a>(mut node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    loop {
        if kinds.contains(&node.kind()) {
            return Some(node);
        }
        node = node.parent()?;
    }
}
const TYPES: &[&str] = &[
    "class_declaration",
    "class_definition",
    "interface_declaration",
    "trait_item",
    "struct_item",
    "type_spec",
    "object_declaration",
];
const FUNCTIONS: &[&str] = &[
    "function_declaration",
    "function_definition",
    "function_item",
    "method_definition",
    "method_signature",
    "method_spec",
    "method_declaration",
    "function_signature_item",
    "arrow_function",
    "lambda_expression",
];
fn named(node: Node<'_>, source: &str) -> Option<String> {
    field(node, &["name"]).map(|name| text(name, source).to_string())
}
fn owner(node: Node<'_>, source: &str, language: &str) -> Option<(String, Option<String>)> {
    if language == "rust" {
        if let Some(implementation) = enclosing(node, &["impl_item"]) {
            return Some((
                type_name(text(field(implementation, &["type"])?, source)),
                field(implementation, &["trait"]).map(|n| type_name(text(n, source))),
            ));
        }
    }
    if language == "go" {
        if let Some(method) = enclosing(node, &["method_declaration"]) {
            let receiver = field(method, &["receiver"])?;
            let parameter = descendants(receiver)
                .into_iter()
                .find(|n| n.kind() == "parameter_declaration")?;
            return Some((type_name(text(field(parameter, &["type"])?, source)), None));
        }
    }
    enclosing(node, TYPES)
        .and_then(|n| named(n, source))
        .map(|n| (n, None))
}

/// All source reads occur here, before immutable blob publication.
pub fn extract(
    source: &str,
    lang: LangId,
    parse: &ParseBlob,
) -> Result<DispatchFacts, ManifestJoinError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&grammar_for(lang))
        .map_err(|e| ManifestJoinError::Parse(e.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ManifestJoinError::Parse("missing dispatch syntax tree".into()))?;
    let all = descendants(tree.root_node());
    let language = parse.language.as_str();
    let mut facts = DispatchFacts::default();
    for node in &all {
        if !TYPES.contains(&node.kind()) {
            continue;
        }
        let Some(name) = named(*node, source) else {
            continue;
        };
        let body = field(*node, &["body", "type"]);
        let interface = node.kind().contains("interface")
            || node.kind() == "trait_item"
            || body.is_some_and(|n| n.kind() == "interface_type")
            || text(*node, source)
                .trim_start()
                .starts_with("abstract class");
        let mut bases = Vec::new();
        for child in node.named_children(&mut node.walk()) {
            if [
                "class_heritage",
                "superclasses",
                "super_interfaces",
                "superclass",
                "delegation_specifiers",
            ]
            .contains(&child.kind())
            {
                for base in descendants(child) {
                    if ["type_identifier", "identifier", "user_type"].contains(&base.kind()) {
                        bases.push(type_name(text(base, source)));
                    }
                }
            }
        }
        if language == "python" {
            if let Some(arguments) = field(*node, &["superclasses"]) {
                bases.extend(
                    arguments
                        .named_children(&mut arguments.walk())
                        .map(|n| type_name(text(n, source))),
                );
            }
        }
        if language == "go" {
            if let Some(body) = body {
                for member in descendants(body) {
                    if member.kind() == "field_declaration" && field(member, &["name"]).is_none() {
                        if let Some(t) = field(member, &["type"]) {
                            bases.push(type_name(text(t, source)));
                        }
                    }
                }
            }
        }
        bases.retain(|name| !name.is_empty());
        bases.sort();
        bases.dedup();
        let header = text(*node, source).split('{').next().unwrap_or_default();
        facts.types.push(TypeHint {
            name,
            bases,
            interface,
            closed: header
                .split_whitespace()
                .any(|w| w == "final" || w == "sealed"),
        });
    }
    for symbol in &parse.symbols {
        let Some(node) = symbol_node(&all, symbol) else {
            continue;
        };
        if !["method", "function"].contains(&symbol.kind.as_str()) {
            continue;
        }
        let Some((owner, trait_name)) = owner(node, source, language) else {
            continue;
        };
        facts.methods.push(MethodHint {
            owner,
            trait_name,
            name: symbol.name.clone(),
            symbol: symbol.scoped_name.clone(),
            has_body: field(node, &["body"]).is_some(),
        });
    }
    // Some interface grammars expose method signatures without callable symbol
    // metadata. Their declaration still has to be a possible exact target.
    for node in &all {
        if ![
            "method_signature",
            "function_signature_item",
            "method_spec",
            "method_declaration",
        ]
        .contains(&node.kind())
        {
            continue;
        }
        let Some((owner, trait_name)) = owner(*node, source, language) else {
            continue;
        };
        let Some(name) = named(*node, source) else {
            continue;
        };
        if facts
            .methods
            .iter()
            .any(|m| m.owner == owner && m.name == name && m.trait_name == trait_name)
        {
            continue;
        }
        if let Some(symbol) = parse
            .symbols
            .iter()
            .find(|s| s.name == name && s.start_line == node.start_position().row as u32)
        {
            facts.methods.push(MethodHint {
                owner,
                trait_name,
                name,
                symbol: symbol.scoped_name.clone(),
                has_body: field(*node, &["body"]).is_some(),
            });
        }
    }
    for reference in &parse.refs {
        if reference.kind != BlobRefKind::Call {
            continue;
        }
        let Some(call) = all
            .iter()
            .filter(|n| {
                crate::calls::call_node_kinds(lang).contains(&n.kind())
                    && n.start_byte() <= reference.byte_start
                    && n.end_byte() >= reference.byte_end
            })
            .min_by_key(|n| n.end_byte() - n.start_byte())
            .copied()
        else {
            continue;
        };
        let Some(callee) = field(call, &["function", "name"]) else {
            continue;
        };
        let dynamic = syntactic_dynamic(callee, source, language);
        let receiver_node = field(callee, &["object", "value", "operand"]);
        // Java method invocations carry their receiver on the invocation itself.
        let receiver_node = receiver_node.or_else(|| field(call, &["object"]));
        if !dynamic && receiver_node.is_none() {
            continue;
        }
        let receiver = receiver_node
            .and_then(|receiver| receiver_type(receiver, call, source, language, &facts));
        facts.sites.push(SiteHint {
            ordinal: reference.ordinal,
            caller: reference.caller_symbol.clone(),
            line: reference.line,
            member: if dynamic {
                None
            } else {
                reference.short_name.clone()
            },
            receiver,
            dynamic,
        });
    }
    // Computed calls may have no extracted callee name and hence no BlobRef.
    for call in &all {
        if !crate::calls::call_node_kinds(lang).contains(&call.kind()) {
            continue;
        }
        let Some(callee) = field(*call, &["function", "name"]) else {
            continue;
        };
        if !syntactic_dynamic(callee, source, language) {
            continue;
        }
        let ordinal = parse
            .ast_nodes
            .iter()
            .filter(|n| n.byte_start <= call.start_byte() && n.byte_end >= call.end_byte())
            .min_by_key(|n| n.byte_end - n.byte_start)
            .map(|n| n.ordinal)
            .unwrap_or_default();
        if facts.sites.iter().any(|s| s.ordinal == ordinal) {
            continue;
        }
        let caller = parse
            .symbols
            .iter()
            .filter(|s| {
                s.start_line <= call.start_position().row as u32
                    && s.end_line >= call.end_position().row as u32
                    && ["method", "function"].contains(&s.kind.as_str())
            })
            .min_by_key(|s| s.end_line - s.start_line)
            .map(|s| s.scoped_name.clone());
        facts.sites.push(SiteHint {
            ordinal,
            caller,
            line: call.start_position().row as u32 + 1,
            member: None,
            receiver: None,
            dynamic: true,
        });
    }
    facts.sites.sort_by_key(|s| s.ordinal);
    facts.sites.dedup_by_key(|s| s.ordinal);
    facts.methods.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    facts.methods.dedup();
    Ok(facts)
}

fn symbol_node<'a>(all: &[Node<'a>], symbol: &BlobSymbol) -> Option<Node<'a>> {
    all.iter()
        .filter(|n| {
            FUNCTIONS.contains(&n.kind())
                && n.start_position().row as u32 == symbol.start_line
                && n.start_position().column as u32 == symbol.start_col
        })
        .min_by_key(|n| n.end_byte() - n.start_byte())
        .copied()
}
fn syntactic_dynamic(callee: Node<'_>, source: &str, language: &str) -> bool {
    match language {
        "javascript" | "typescript" | "tsx" => callee.kind() == "subscript_expression",
        "python" => {
            callee.kind() == "call"
                && field(callee, &["function"]).is_some_and(|n| text(n, source) == "getattr")
        }
        _ => false,
    }
}

fn receiver_type(
    receiver: Node<'_>,
    call: Node<'_>,
    source: &str,
    language: &str,
    facts: &DispatchFacts,
) -> Option<String> {
    let value = text(receiver, source);
    let function = enclosing(call, FUNCTIONS)?;
    let current_owner = owner(function, source, language).map(|(o, _)| o);
    match language {
        "typescript" | "tsx" | "javascript" | "java" | "csharp" | "kotlin" if value == "this" => {
            return current_owner
        }
        "python" if value == "self" => return current_owner,
        "python" if value == "cls" => {
            let decorated = function
                .parent()
                .filter(|p| p.kind() == "decorated_definition")?;
            if text(decorated, source).contains("@classmethod") {
                return current_owner;
            }
            return None;
        }
        "rust" if value == "self" => return current_owner,
        "typescript" | "tsx" | "javascript" | "java" | "csharp" | "kotlin" if value == "super" => {
            return facts
                .types
                .iter()
                .find(|t| Some(&t.name) == current_owner.as_ref())?
                .bases
                .first()
                .cloned()
        }
        "python" if value == "super()" => {
            return facts
                .types
                .iter()
                .find(|t| Some(&t.name) == current_owner.as_ref())?
                .bases
                .first()
                .cloned()
        }
        "go" => {
            if let Some(receiver) = field(function, &["receiver"]) {
                for parameter in descendants(receiver) {
                    if parameter.kind() == "parameter_declaration"
                        && field(parameter, &["name"]).is_some_and(|n| text(n, source) == value)
                    {
                        return field(parameter, &["type"]).map(|n| type_name(text(n, source)));
                    }
                }
            }
        }
        _ => {}
    }
    if ![
        "typescript",
        "tsx",
        "javascript",
        "python",
        "rust",
        "go",
        "java",
        "csharp",
        "kotlin",
    ]
    .contains(&language)
    {
        return None;
    }
    if !["identifier", "self"].contains(&receiver.kind()) {
        return None;
    }
    // Only declarations in the enclosing function are evidence. Assignments
    // invalidate constructor inference; arbitrary return-value inference is absent.
    let nodes = descendants(function);
    for declaration in &nodes {
        if ![
            "required_parameter",
            "optional_parameter",
            "typed_parameter",
            "typed_default_parameter",
            "parameter",
            "parameter_declaration",
            "formal_parameter",
            "variable_declarator",
            "let_declaration",
            "var_spec",
            "short_var_declaration",
            "assignment",
            "local_variable_declaration",
            "property_declaration",
        ]
        .contains(&declaration.kind())
        {
            continue;
        }
        let binding = field(*declaration, &["name", "pattern", "left"]);
        let Some(binding) = binding else {
            continue;
        };
        let binding_text = text(binding, source).trim();
        if binding_text != value && !binding_text.strip_suffix(':').is_some_and(|s| s == value) {
            continue;
        }
        if let Some(annotation) = field(*declaration, &["type"]) {
            if language != "javascript" {
                return Some(type_name(text(annotation, source)));
            }
        }
        // TS annotations are type_annotation children rather than fields on some bindings.
        if language == "typescript" || language == "tsx" {
            if let Some(annotation) = declaration
                .named_children(&mut declaration.walk())
                .find(|n| n.kind() == "type_annotation")
            {
                return Some(type_name(text(annotation, source)));
            }
        }
        if declaration.start_byte() > call.start_byte() {
            continue;
        }
        let assignments = nodes
            .iter()
            .filter(|n| {
                [
                    "assignment_expression",
                    "augmented_assignment_expression",
                    "assignment",
                    "augmented_assignment",
                    "assignment_statement",
                    "short_var_declaration",
                    "update_expression",
                ]
                .contains(&n.kind())
                    && field(**n, &["left", "argument"])
                        .is_some_and(|lhs| text(lhs, source).trim() == value)
            })
            .count();
        if (language == "python" || language == "go") && assignments > 1
            || !["python", "go"].contains(&language) && assignments > 0
        {
            return None;
        }
        let initializer = field(*declaration, &["value", "right"])?;
        let initializer = if initializer.kind() == "expression_list" {
            initializer.named_child(0)?
        } else {
            initializer
        };
        match language {
            "typescript" | "tsx" | "javascript" | "java" | "csharp" | "kotlin"
                if initializer.kind() == "new_expression"
                    || initializer.kind() == "object_creation_expression" =>
            {
                return field(initializer, &["constructor", "type"])
                    .map(|n| type_name(text(n, source)))
            }
            "python" if initializer.kind() == "call" => {
                let constructor = field(initializer, &["function"])?;
                let candidate = text(constructor, source);
                if facts.types.iter().any(|t| t.name == candidate) {
                    return Some(candidate.to_string());
                }
            }
            "go" => {
                if let Some(literal) = descendants(initializer)
                    .into_iter()
                    .find(|n| n.kind() == "composite_literal")
                {
                    return field(literal, &["type"]).map(|n| type_name(text(n, source)));
                }
            }
            "rust" => {
                if initializer.kind() == "struct_expression" {
                    return field(initializer, &["name"]).map(|n| type_name(text(n, source)));
                }
                if initializer.kind() == "call_expression" {
                    let name = text(field(initializer, &["function"])?, source);
                    let (ty, method) = name.split_once("::")?;
                    if method != "new" {
                        return None;
                    }
                    let valid = descendants(enclosing(function, &["source_file"])?)
                        .into_iter()
                        .any(|n| {
                            n.kind() == "function_item"
                                && named(n, source).as_deref() == Some("new")
                                && owner(n, source, language).is_some_and(|(o, _)| o == ty)
                                && field(n, &["return_type"])
                                    .is_some_and(|t| ["Self", ty].contains(&text(t, source)))
                        });
                    if valid {
                        return Some(ty.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Target {
    pub file: String,
    pub symbol: String,
    pub provenance: &'static str,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Resolution {
    pub targets: BTreeSet<Target>,
    pub protected: BTreeSet<(String, String)>,
    pub unresolved: usize,
    pub external: usize,
    pub dynamic: usize,
}

/// The ruled resolver operates on a complete manifest's immutable hints. Unknown
/// receivers protect members by written name, but never produce graph edges.
pub struct Resolver<'a> {
    pub files: BTreeMap<String, &'a ParseBlob>,
}
impl Resolver<'_> {
    fn project_type(&self, file: &str, name: &str) -> Option<(String, &TypeHint)> {
        let parse = self.files.get(file)?;
        if let Some(t) = parse.dispatch.types.iter().find(|t| t.name == name) {
            return Some((file.to_string(), t));
        }
        // Relative imports are resolved only to manifest members. Library imports
        // are not allowed to bind a coincidentally same-named project type.
        for import in &parse.imports {
            if !import
                .names
                .iter()
                .any(|n| n.split_whitespace().last() == Some(name))
                && import.default_import.as_deref() != Some(name)
            {
                continue;
            }
            if !import.module_path.starts_with('.') {
                return None;
            }
            let parent = std::path::Path::new(file).parent()?;
            let mut parts = Vec::new();
            let joined = parent.join(&import.module_path);
            for c in joined.components() {
                match c {
                    std::path::Component::ParentDir => {
                        parts.pop();
                    }
                    std::path::Component::Normal(p) => parts.push(p.to_string_lossy().to_string()),
                    _ => {}
                }
            }
            let base = parts.join("/");
            for (path, candidate) in &self.files {
                if candidate.language != parse.language {
                    continue;
                }
                if path == &base
                    || ["ts", "tsx", "js", "py", "rs", "go"]
                        .iter()
                        .any(|ext| path == &format!("{base}.{ext}"))
                {
                    if let Some(t) = candidate.dispatch.types.iter().find(|t| t.name == name) {
                        return Some((path.clone(), t));
                    }
                }
            }
            return None;
        }
        None
    }
    fn methods(&self, file: &str, owner: &str, name: &str) -> Vec<Target> {
        self.files[file]
            .dispatch
            .methods
            .iter()
            .filter(|m| m.owner == owner && m.name == name)
            .map(|m| Target {
                file: file.to_string(),
                symbol: m.symbol.clone(),
                provenance: "exact",
            })
            .collect()
    }
    fn nearest(
        &self,
        file: &str,
        ty: &TypeHint,
        name: &str,
        seen: &mut BTreeSet<(String, String)>,
    ) -> Vec<Target> {
        if !seen.insert((file.to_string(), ty.name.clone())) {
            return Vec::new();
        }
        let direct = self.methods(file, &ty.name, name);
        if !direct.is_empty() {
            return direct;
        }
        let mut frontier = ty
            .bases
            .iter()
            .filter_map(|base| self.project_type(file, base))
            .collect::<Vec<_>>();
        while !frontier.is_empty() {
            let mut hits = Vec::new();
            let mut next = Vec::new();
            for (file, ty) in frontier {
                if !seen.insert((file.clone(), ty.name.clone())) {
                    continue;
                }
                hits.extend(self.methods(&file, &ty.name, name));
                next.extend(
                    ty.bases
                        .iter()
                        .filter_map(|base| self.project_type(&file, base)),
                );
            }
            if !hits.is_empty() {
                return hits;
            }
            frontier = next;
        }
        Vec::new()
    }
    fn subtype(
        &self,
        file: &str,
        ty: &TypeHint,
        base_file: &str,
        base: &str,
        seen: &mut BTreeSet<(String, String)>,
    ) -> bool {
        if !seen.insert((file.to_string(), ty.name.clone())) {
            return false;
        }
        ty.bases
            .iter()
            .filter_map(|b| self.project_type(file, b))
            .any(|(f, t)| {
                (f == base_file && t.name == base) || self.subtype(&f, t, base_file, base, seen)
            })
    }
    fn unknown(&self, language: &str, member: &str) -> Resolution {
        let protected: BTreeSet<_> = self
            .files
            .iter()
            .filter(|(_, p)| p.language == language)
            .flat_map(|(file, p)| {
                p.dispatch
                    .methods
                    .iter()
                    .filter(move |m| m.name == member)
                    .map(move |m| (file.clone(), m.symbol.clone()))
            })
            .collect();
        Resolution {
            unresolved: usize::from(!protected.is_empty()),
            external: usize::from(protected.is_empty()),
            protected,
            ..Resolution::default()
        }
    }
    pub fn resolve(&self, file: &str, site: &SiteHint) -> Resolution {
        let parse = self.files[file];
        if site.dynamic {
            return Resolution {
                dynamic: 1,
                ..Resolution::default()
            };
        }
        let Some(member) = &site.member else {
            return Resolution::default();
        };
        let Some(receiver) = &site.receiver else {
            return self.unknown(&parse.language, member);
        };
        let Some((type_file, ty)) = self.project_type(file, receiver) else {
            return Resolution {
                external: 1,
                ..Resolution::default()
            };
        };
        let mut exact = self.nearest(&type_file, ty, member, &mut BTreeSet::new());
        if parse.language == "rust" {
            let methods = self.files[&type_file]
                .dispatch
                .methods
                .iter()
                .filter(|m| m.owner == ty.name && m.name == *member)
                .collect::<Vec<_>>();
            let inherent = methods
                .iter()
                .filter(|m| m.trait_name.is_none())
                .collect::<Vec<_>>();
            if !inherent.is_empty() {
                exact = inherent
                    .iter()
                    .map(|m| Target {
                        file: type_file.clone(),
                        symbol: m.symbol.clone(),
                        provenance: "exact",
                    })
                    .collect();
            } else if methods.len() > 1 {
                return self.unknown(&parse.language, member);
            }
        }
        if parse.language == "go" && exact.len() > 1 {
            return self.unknown(&parse.language, member);
        }
        let mut targets = exact.into_iter().collect::<BTreeSet<_>>();
        if ty.interface || (!ty.closed && !["rust", "go"].contains(&parse.language.as_str())) {
            for (candidate_file, candidate) in &self.files {
                if candidate.language != parse.language {
                    continue;
                }
                for method in &candidate.dispatch.methods {
                    if method.name != *member {
                        continue;
                    }
                    let implements = method.trait_name.as_deref() == Some(&ty.name)
                        || candidate
                            .dispatch
                            .types
                            .iter()
                            .find(|t| t.name == method.owner)
                            .is_some_and(|sub| {
                                self.subtype(
                                    candidate_file,
                                    sub,
                                    &type_file,
                                    &ty.name,
                                    &mut BTreeSet::new(),
                                )
                            })
                        || (parse.language == "go" && ty.interface && {
                            let required = self.files[&type_file]
                                .dispatch
                                .methods
                                .iter()
                                .filter(|m| m.owner == ty.name)
                                .map(|m| &m.name)
                                .collect::<BTreeSet<_>>();
                            !required.is_empty()
                                && required.iter().all(|name| {
                                    candidate
                                        .dispatch
                                        .methods
                                        .iter()
                                        .any(|m| m.owner == method.owner && &m.name == *name)
                                })
                        });
                    if implements && !(candidate_file == &type_file && method.owner == ty.name) {
                        targets.insert(Target {
                            file: candidate_file.clone(),
                            symbol: method.symbol.clone(),
                            provenance: "dispatch",
                        });
                    }
                }
            }
        }
        let external = usize::from(targets.is_empty());
        Resolution {
            targets,
            external,
            ..Resolution::default()
        }
    }
}

#[cfg(test)]
#[path = "dispatch/tests.rs"]
mod tests;
