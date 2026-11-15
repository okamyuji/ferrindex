mod common;
use common::{Repo, SAMPLE};

#[test]
fn qualified_name_returns_only_that_containers_member() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    let run = r.run(&["find", "Foo.bar"]);
    assert_eq!(run.code, 0, "{}", run.err);
    assert_eq!(run.heads(), ["a.rs:3:8 method Foo.bar"]);
    let run = r.run(&["find", "Baz.bar"]);
    assert_eq!(run.heads(), ["a.rs:7:8 method Baz.bar"]);
}

#[test]
fn unqualified_name_returns_every_container() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    let run = r.run(&["find", "bar"]);
    assert_eq!(
        run.heads(),
        ["a.rs:3:8 method Foo.bar", "a.rs:7:8 method Baz.bar"]
    );
}

#[test]
fn wrong_container_is_no_result() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    let run = r.run(&["find", "Nope.bar"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.out, "");
}

#[test]
fn heading_with_a_dot_is_found_by_its_whole_name() {
    let r = Repo::new();
    r.write("doc.md", "# Rust 1.100 notes\n\ntext\n");
    let run = r.run(&["find", "Rust 1.100 notes"]);
    assert_eq!(run.heads(), ["doc.md:1:3 section Rust 1.100 notes"]);
}

#[test]
fn dot_at_either_end_is_part_of_the_name() {
    let r = Repo::new();
    r.write("doc.md", "# Hello. World\n\n# x .bar y\n");
    assert_eq!(
        r.run(&["find", "hello."]).heads(),
        ["doc.md:1:3 section Hello. World"]
    );
    assert_eq!(
        r.run(&["find", ".bar"]).heads(),
        ["doc.md:3:3 section x .bar y"]
    );
}

#[test]
fn trailing_or_leading_dot_is_not_a_qualifier() {
    let r = common::Repo::new();
    r.write("a.rs", common::SAMPLE);
    assert_eq!(r.run(&["find", "Foo."]).code, 1);
    assert_eq!(r.run(&["find", ".bar"]).code, 1);
}
