use super::*;
use crate::callgraph_store::join::CallgraphBlob;

fn parse(source: &str, language: &str) -> ParseBlob {
    match CallgraphBlob::extract(source, language, "ruled-dispatch-1").unwrap() {
        CallgraphBlob::Parse(parse) => parse,
        _ => unreachable!(),
    }
}
fn resolutions(parse: &ParseBlob) -> Vec<Resolution> {
    let resolver = Resolver {
        files: BTreeMap::from([("fixture".into(), parse)]),
    };
    parse
        .dispatch
        .sites
        .iter()
        .map(|site| resolver.resolve("fixture", site))
        .collect()
}

#[test]
fn interface_two_implementations_are_possible_targets() {
    let parse = parse("interface I { m(): void; }\nclass A implements I { m() {} }\nclass B implements I { m() {} }\nfunction caller(x: I) { x.m(); }", "typescript");
    let result = resolutions(&parse);
    assert_eq!(result.len(), 1, "{:#?}", parse.dispatch);
    assert_eq!(result[0].targets.len(), 3, "{:#?}\n{:#?}", parse, result);
    assert_eq!(
        result[0]
            .targets
            .iter()
            .filter(|t| t.provenance == "dispatch")
            .count(),
        2
    );
    assert_eq!(
        result[0]
            .targets
            .iter()
            .filter(|t| t.provenance == "exact")
            .count(),
        1
    );
}

#[test]
fn unknown_arity_and_private_methods_remain_live_without_edges() {
    let parse = parse("class A { m() {} }\nclass B { private m(a: number, b: number) {} n() {} }\nfunction caller(x) { x.m(); }", "typescript");
    let result = resolutions(&parse);
    assert_eq!(result.len(), 1, "{:#?}", parse.dispatch);
    assert!(result[0].targets.is_empty());
    assert_eq!(result[0].unresolved, 1);
    assert_eq!(result[0].protected.len(), 2, "{:#?}", parse.dispatch);
    assert!(result[0].protected.iter().all(|(_, s)| s.ends_with("m")));
}

#[test]
fn unknown_and_dynamic_public_fixture_python_js_ts() {
    for (language, source) in [
        ("typescript", "class A { m() {} }\nclass B { m() {} n() {} }\nfunction caller(x, name) { x.m(); x[name](); }"),
        ("javascript", "class A { m() {} }\nclass B { m() {} n() {} }\nfunction caller(x, name) { x.m(); x[name](); }"),
        ("python", "class A:\n def m(self): pass\nclass B:\n def m(self): pass\n def n(self): pass\ndef caller(x, name):\n x.m()\n getattr(x, name)()\n"),
    ] {
        let parse = parse(source, language);
        let result = resolutions(&parse);
        assert_eq!(result.iter().map(|r| r.unresolved).sum::<usize>(), 1, "{language}: {:#?}", parse.dispatch);
        assert_eq!(result.iter().map(|r| r.dynamic).sum::<usize>(), 1, "{language}: {:#?}", parse.dispatch);
        assert_eq!(result.iter().map(|r| r.external).sum::<usize>(), 0, "{language}: {:#?}", parse.dispatch);
        assert!(result.iter().all(|r| r.targets.is_empty()));
        assert_eq!(result.iter().flat_map(|r| &r.protected).collect::<BTreeSet<_>>().len(), 2);
    }
}

#[test]
fn builtin_type_does_not_link_project_names() {
    let parse = parse(
        "class A { m() {} }\nfunction caller(x: String) { x.m(); }",
        "typescript",
    );
    let result = resolutions(&parse);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].external, 1);
    assert!(result[0].targets.is_empty());
    assert!(result[0].protected.is_empty());
}

#[test]
fn concrete_inherited_and_constructor_receiver_forms() {
    let parse = parse("class A { m() {} }\nclass B extends A { f() { this.m(); super.m(); } }\nclass D extends B { m() {} }\nfunction caller() { const x = new B(); x.m(); }", "typescript");
    let result = resolutions(&parse);
    assert_eq!(result.len(), 3, "{:#?}", parse.dispatch);
    for r in result {
        assert_eq!(r.targets.len(), 2, "{r:#?}\n{:#?}", parse.dispatch);
        assert_eq!(
            r.targets.iter().filter(|t| t.provenance == "exact").count(),
            1
        );
        assert_eq!(
            r.targets
                .iter()
                .filter(|t| t.provenance == "dispatch")
                .count(),
            1
        );
    }
}
