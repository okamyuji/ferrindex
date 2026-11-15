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

#[test]
fn rust_impl_trait_names_the_trait_as_inheritance_of_the_type() {
    let src = "trait Tr {}\nstruct X;\nstruct G<T>(T);\nimpl Tr for X {}\nimpl<T> Tr for G<T> {}\nimpl a::b::Tr for crate::m::X {}\nimpl X {}\n";
    let defs = extract(Lang::Rust, src.as_bytes());
    let got: Vec<(String, u8, u32, Option<String>)> =
        extract_refs(Lang::Rust, src.as_bytes(), &defs)
            .into_iter()
            .filter(|r| r.kind == REF_INHERIT)
            .map(|r| (r.name, r.kind, r.line, r.container))
            .collect();
    assert_eq!(
        got,
        [
            ("Tr".into(), REF_INHERIT, 4, Some("X".into())),
            ("Tr".into(), REF_INHERIT, 5, Some("G".into())),
            ("Tr".into(), REF_INHERIT, 6, Some("X".into())),
        ]
    );
}

#[test]
fn rust_trait_arguments_and_path_segments_stay_plain() {
    let got = names(Lang::Rust, "impl a::Conv<Y> for X {}\n");
    assert_eq!(
        got,
        [
            ("a".into(), REF_PLAIN),
            ("Conv".into(), REF_INHERIT),
            ("Y".into(), REF_PLAIN),
            ("X".into(), REF_PLAIN)
        ]
    );
}

#[test]
fn typescript_qualified_bases_inherit_only_the_last_name() {
    let src = "class F extends ns.Base implements I1, ns.a.I2 {}\ninterface J extends K, ns.L {}\n";
    let inherit: Vec<(String, u8)> = names(Lang::TypeScript, src);
    assert_eq!(
        inherit,
        [
            ("ns".into(), REF_PLAIN),
            ("Base".into(), REF_INHERIT),
            ("I1".into(), REF_INHERIT),
            ("ns".into(), REF_PLAIN),
            ("a".into(), REF_PLAIN),
            ("I2".into(), REF_INHERIT),
            ("K".into(), REF_INHERIT),
            ("ns".into(), REF_PLAIN),
            ("L".into(), REF_INHERIT),
        ]
    );
}

#[test]
fn rust_impl_for_a_reference_type_names_the_referent() {
    let src = "struct S<T>(T);\nimpl Tr for &S<u8> {}\nimpl Tr for &mut S<u8> {}\nimpl Tr for (S<u8>, u8) {}\n";
    let defs = extract(Lang::Rust, src.as_bytes());
    let got: Vec<(u32, Option<String>)> = extract_refs(Lang::Rust, src.as_bytes(), &defs)
        .into_iter()
        .filter(|r| r.kind == REF_INHERIT)
        .map(|r| (r.line, r.container))
        .collect();
    assert_eq!(
        got,
        [(2, Some("S".into())), (3, Some("S".into())), (4, None)]
    );
}
