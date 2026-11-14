mod common;
use std::path::Path;

use common::{Repo, SAMPLE};

#[test]
fn show_prints_header_and_current_body() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    let run = r.run(&["show", "Foo.bar"]);
    assert_eq!(run.code, 0, "{}", run.err);
    assert_eq!(
        run.lines(),
        ["a.rs:3-3 method Foo.bar", "    fn bar(&self) {}"]
    );
}

#[test]
fn show_uses_fresh_line_numbers_after_an_edit() {
    let r = Repo::new();
    r.write("a.rs", "fn target() {\n    1;\n}\n");
    assert_eq!(r.run(&["find", "target"]).code, 0);
    r.write("a.rs", "// pad\n// pad\nfn target() {\n    1;\n    2;\n}\n");
    let run = r.run(&["show", "target"]);
    assert_eq!(
        run.lines(),
        [
            "a.rs:3-6 function target",
            "fn target() {",
            "    1;",
            "    2;",
            "}"
        ]
    );
}

#[test]
fn show_default_limit_is_3_and_reports_the_rest() {
    let r = Repo::new();
    for i in 0..5 {
        r.write(&format!("f{i}.rs"), "fn same() {}\n");
    }
    let run = r.run(&["show", "same"]);
    let heads = run
        .lines()
        .iter()
        .filter(|l| l.contains(" function same"))
        .count();
    assert_eq!(heads, 3);
    assert_eq!(run.err, "… 2 more\n");
    let one = r.run(&["show", "same", "--limit", "1"]);
    assert_eq!(one.lines(), ["f0.rs:1-1 function same", "fn same() {}"]);
    assert_eq!(one.err, "… 4 more\n");
}

#[test]
fn show_truncates_at_200_lines() {
    let r = Repo::new();
    let mut text = String::from("fn long() {\n");
    for i in 0..249 {
        text.push_str(&format!("    let a{i} = {i};\n"));
    }
    text.push_str("}\n");
    r.write("a.rs", &text);
    let run = r.run(&["show", "long"]);
    let lines = run.lines();
    assert_eq!(lines[0], "a.rs:1-251 function long");
    assert_eq!(lines.len(), 1 + 200 + 1);
    assert_eq!(lines[200], "    let a198 = 198;");
    assert_eq!(lines[201], "… (51 more lines)");
    let exact = Repo::new();
    let mut t = String::from("fn len200() {\n");
    for i in 0..198 {
        t.push_str(&format!("    let a{i} = {i};\n"));
    }
    t.push_str("}\n");
    exact.write("a.rs", &t);
    let run = exact.run(&["show", "len200"]);
    assert_eq!(run.lines().len(), 1 + 200);
    assert!(!run.out.contains("more lines"));
}

#[test]
fn show_header_path_is_cwd_relative() {
    let r = Repo::new();
    r.write("sub/in.rs", "fn target() {}\n");
    let run = r.run_in(Path::new("sub"), &["show", "target"]);
    assert_eq!(run.lines(), ["in.rs:1-1 function target", "fn target() {}"]);
}

#[test]
fn show_markdown_section_runs_to_next_heading() {
    let r = Repo::new();
    r.write("d.md", "# One\nalpha\n\n# Two\nbeta\n");
    let run = r.run(&["show", "One"]);
    assert_eq!(run.lines(), ["d.md:1-3 section One", "# One", "alpha", ""]);
}

#[test]
fn names_and_descriptions_never_print_raw_control_characters() {
    let repo = Repo::new();
    repo.write("a.md", "# Head\x1b[31mRED\x1b]0;PWNED\x07 end\n\nbody\n");
    repo.write("b.rs", "// note\x1b[2J here\nfn plain() {}\n");
    for args in [
        &["find", "Head"][..],
        &["outline", "a.md"],
        &["show", "Head"],
        &["map"],
    ] {
        let run = repo.run(args);
        assert_eq!(run.code, 0, "{args:?}: {}", run.err);
        // show の本文はファイルの中身なので、見出し行だけを調べる
        let checked = if args[0] == "show" {
            run.out.lines().next().unwrap_or("").to_string()
        } else {
            run.out.clone()
        };
        assert!(
            !checked.contains('\x1b') && !checked.contains('\x07'),
            "{args:?}: {checked:?}"
        );
        assert!(checked.contains("\\x1b"), "{args:?}: {checked:?}");
    }
}
