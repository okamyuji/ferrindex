mod common;

use common::Repo;

const RUST_SRC: &str = "// target is mentioned here
fn target() {}
fn caller_fn() {
    target();
    let s = \"target\";
    let f = target;
    external_call();
}
";

const PY_SRC: &str = "class A:\n    pass\n\nclass B(A):\n    pass\n";

fn repo() -> Repo {
    let r = Repo::new();
    r.write("a.rs", RUST_SRC);
    r.write("b.py", PY_SRC);
    r
}

#[test]
fn refs_list_code_occurrences_only() {
    let r = repo();
    let o = r.run(&["refs", "target"]);
    assert_eq!(o.code, 0, "{}", o.err);
    assert_eq!(
        o.lines(),
        [
            "a.rs:4:5 call in caller_fn\ttarget();",
            "a.rs:6:13 ref in caller_fn\tlet f = target;",
        ]
    );
}

#[test]
fn callers_show_the_calling_function() {
    let r = repo();
    let o = r.run(&["callers", "target"]);
    assert_eq!(o.code, 0, "{}", o.err);
    assert_eq!(o.lines(), ["a.rs:4:5 call in caller_fn\ttarget();"]);
}

#[test]
fn impls_list_subclasses() {
    let r = repo();
    let o = r.run(&["impls", "A"]);
    assert_eq!(o.code, 0, "{}", o.err);
    assert_eq!(o.lines(), ["b.py:4:9 inherit in B\tclass B(A):"]);
}

#[test]
fn supers_list_bases() {
    let r = repo();
    let o = r.run(&["supers", "B"]);
    assert_eq!(o.code, 0, "{}", o.err);
    assert_eq!(o.lines(), ["b.py:4:9 inherit in B\tclass B(A):"]);
}

#[test]
fn qualified_term_uses_the_last_segment() {
    let r = Repo::new();
    r.write("a.rs", "fn user() {\n    x.bar();\n}\n");
    for term in ["Foo.bar", "Foo::bar", "bar"] {
        let o = r.run(&["refs", term]);
        assert_eq!(o.lines(), ["a.rs:2:7 call in user\tx.bar();"], "{term}");
    }
}

#[test]
fn no_references_exits_1() {
    let r = repo();
    let o = r.run(&["callers", "nothing_calls_this"]);
    assert_eq!((o.code, o.out.as_str(), o.err.as_str()), (1, "", ""));
    assert_eq!(r.run(&["refs", "nothing_calls_this"]).code, 1);
    assert_eq!(r.run(&["impls", "B"]).code, 1);
    assert_eq!(r.run(&["supers", "A"]).code, 1);
}

#[test]
fn reference_only_name_is_found() {
    let r = repo();
    let o = r.run(&["callers", "external_call"]);
    assert_eq!(o.lines(), ["a.rs:7:5 call in caller_fn\texternal_call();"]);
    let o = r.run(&["refs", "external_call"]);
    assert_eq!(o.lines(), ["a.rs:7:5 call in caller_fn\texternal_call();"]);
}

fn many_calls(n: usize) -> String {
    let mut s = String::from("fn caller_fn() {\n");
    for _ in 0..n {
        s.push_str("    target();\n");
    }
    s.push_str("}\n");
    s
}

#[test]
fn limit_and_more_line() {
    let r = Repo::new();
    r.write("a.rs", &many_calls(25));
    for cmd in ["callers", "refs"] {
        let o = r.run(&[cmd, "target"]);
        assert_eq!(o.lines().len(), 20, "{cmd}");
        assert_eq!(o.err, "… 5 more\n", "{cmd}");
        let o = r.run(&[cmd, "target", "--limit", "25"]);
        assert_eq!((o.lines().len(), o.err.as_str()), (25, ""), "{cmd}");
        let o = r.run(&[cmd, "target", "--limit", "2"]);
        assert_eq!(
            (o.lines().len(), o.err.as_str()),
            (2, "… 23 more\n"),
            "{cmd}"
        );
    }
}

#[test]
fn refs_count_the_excess_when_every_file_was_read() {
    let r = Repo::new();
    for f in ["a", "b", "c"] {
        r.write(&format!("{f}.rs"), "fn x() {\n    target();\n}\n");
    }
    r.write("d.rs", "fn unrelated() {}\n");
    let o = r.run(&["refs", "target", "--limit", "1"]);
    assert_eq!(o.lines(), ["a.rs:2:5 call in x\ttarget();"]);
    assert_eq!(o.err, "… 2 more\n");
}

#[test]
fn refs_skip_binary_and_oversize_files() {
    let r = Repo::new();
    r.write("a.rs", "fn x() {\n    target();\n}\n");
    r.write("bin.rs", "\0 fn y() {\n    target();\n}\n");
    r.write(
        "big.rs",
        &format!("fn z() {{ target(); }}\n{}", " ".repeat(1 << 20)),
    );
    let o = r.run(&["refs", "target"]);
    assert_eq!(o.lines(), ["a.rs:2:5 call in x\ttarget();"]);
    assert_eq!(o.err, "");
}

#[test]
fn refs_are_fresh_after_edit() {
    let r = repo();
    r.run(&["update"]);
    r.write(
        "a.rs",
        &format!("{RUST_SRC}fn later() {{\n    target();\n}}\n"),
    );
    for cmd in ["callers", "refs"] {
        let o = r.run(&[cmd, "target"]);
        assert!(
            o.out.contains("a.rs:10:5 call in later"),
            "{cmd}: {}",
            o.out
        );
    }
}

#[test]
fn paths_are_relative_to_the_current_directory() {
    let r = Repo::new();
    r.write("sub/a.rs", "fn x() {\n    target();\n}\n");
    r.write("top.rs", "fn y() {\n    target();\n}\n");
    for cmd in ["callers", "refs"] {
        let o = r.run_in(std::path::Path::new("sub"), &[cmd, "target"]);
        assert_eq!(
            o.lines(),
            [
                "a.rs:2:5 call in x\ttarget();",
                "../top.rs:2:5 call in y\ttarget();"
            ],
            "{cmd}"
        );
    }
}

#[test]
fn bad_arguments_exit_2() {
    let r = repo();
    assert_eq!(r.run(&["refs"]).code, 2);
    assert_eq!(r.run(&["refs", "a", "b"]).code, 2);
    assert_eq!(r.run(&["refs", "  "]).code, 2);
    assert_eq!(r.run(&["refs", "target", "--precise"]).code, 2);
}

#[test]
fn refs_keep_path_order_across_parallel_batches() {
    let r = Repo::new();
    for i in 0..1000 {
        r.write(&format!("d{i:03}.rs"), "fn x() {\n    target();\n}\n");
    }
    let o = r.run(&["refs", "target", "--limit", "300"]);
    let paths: Vec<String> = o
        .lines()
        .iter()
        .map(|l| l.split(':').next().unwrap().to_string())
        .collect();
    let want: Vec<String> = (0..300).map(|i| format!("d{i:03}.rs")).collect();
    assert_eq!(paths, want);
    assert_eq!(o.err, "… more files may match\n");
    let all = r.run(&["refs", "target", "--limit", "1000"]);
    assert_eq!((all.lines().len(), all.err.as_str()), (1000, ""));
}

const RUST_IMPLS: &str = "trait Tr {}
struct X;
struct G<T>(T);
impl Tr for X {
    fn f(&self) {}
}
impl<T> Tr for G<T> {}
";

#[test]
fn rust_impls_and_supers_follow_impl_trait_for() {
    let r = Repo::new();
    r.write("a.rs", RUST_IMPLS);
    let o = r.run(&["impls", "Tr"]);
    assert_eq!(
        o.lines(),
        [
            "a.rs:4:6 inherit in X\timpl Tr for X {",
            "a.rs:7:9 inherit in G\timpl<T> Tr for G<T> {}"
        ]
    );
    let o = r.run(&["supers", "G"]);
    assert_eq!(o.lines(), ["a.rs:7:9 inherit in G\timpl<T> Tr for G<T> {}"]);
    assert_eq!(r.run(&["supers", "Tr"]).code, 1);
}

#[test]
fn go_impls_and_supers_ask_for_precise() {
    let r = Repo::new();
    r.write(
        "a.go",
        "package a\n\ntype Reader interface {\n\tRead()\n}\n\ntype F struct{}\n",
    );
    for args in [["impls", "Reader"], ["supers", "F"]] {
        let o = r.run(&args);
        assert_eq!(o.code, 1, "{args:?}");
        assert_eq!(o.out, "");
        assert_eq!(
            o.err,
            "ferrindex: Go のインターフェースの実装は名前一致では分かりません。--precise を使ってください\n"
        );
    }
    // callers は Go でも名前一致で答えられる
    r.write("b.go", "package a\n\nfunc g() {\n\tx.Read()\n}\n");
    assert_eq!(r.run(&["callers", "Read"]).code, 0);
}
