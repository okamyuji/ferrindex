use ferrindex::extract::extract;
use ferrindex::lang::Lang;
use ferrindex::refs::{REF_INHERIT, REF_PLAIN, extract_refs};

/// 継承の関係に現れる名前を (名前, 種類) で返す。
fn names(lang: Lang, src: &str) -> Vec<(String, u8)> {
    let defs = extract(lang, src.as_bytes());
    extract_refs(lang, src.as_bytes(), &defs)
        .into_iter()
        .map(|r| (r.name, r.kind))
        .collect()
}

#[test]
fn ruby_scope_resolution_inherits_only_the_last_name() {
    let got = names(Lang::Ruby, "class A < Api::V1::BaseController\nend\n");
    assert_eq!(
        got,
        [
            ("Api".into(), REF_PLAIN),
            ("V1".into(), REF_PLAIN),
            ("BaseController".into(), REF_INHERIT)
        ]
    );
}

#[test]
fn java_qualified_base_inherits_only_the_last_name() {
    let got = names(Lang::Java, "class A extends a.b.I {}\n");
    assert_eq!(
        got,
        [
            ("a".into(), REF_PLAIN),
            ("b".into(), REF_PLAIN),
            ("I".into(), REF_INHERIT)
        ]
    );
}

#[test]
fn cpp_qualified_base_inherits_only_the_last_name() {
    let got = names(Lang::Cpp, "class A : public ns::B {};\n");
    assert_eq!(got, [("ns".into(), REF_PLAIN), ("B".into(), REF_INHERIT)]);
}
