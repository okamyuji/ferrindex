mod common;

use std::{fs, path::PathBuf};

use common::{Repo, SAMPLE};

const HEADER_LEN: usize = 108;
const SECTIONS: usize = 7;
const SECTION_TABLE_AT: usize = 20;
const SECTION_ENTRY_LEN: usize = 12;

fn index_files(repo: &Repo) -> (PathBuf, PathBuf) {
    let dir = fs::read_dir(repo.xdg.join("ferrindex"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    (dir.join("main.fidx"), dir.join("delta.fidx"))
}

/// 本体と差分区画の両方がある状態を作る。
fn repo_with_delta() -> Repo {
    let repo = Repo::new();
    repo.write("a.rs", SAMPLE);
    repo.write("b.rs", "fn second() {}\n");
    for i in 0..12 {
        repo.write(
            &format!("filler/f{i}.rs"),
            &format!("fn filler_{i}() {{}}\n"),
        );
    }
    repo.age();
    assert_eq!(repo.run(&["find", "free_fn"]).code, 0);
    repo.write("b.rs", "fn second() {}\nfn added_later() {}\n");
    repo.age();
    assert_eq!(repo.run(&["find", "added_later"]).code, 0);
    assert!(index_files(&repo).1.exists(), "the delta file must exist");
    repo
}

const QUERIES: [&[&str]; 4] = [
    &["find", "free_fn"],
    &["find", "added_later"],
    &["show", "second"],
    &["map"],
];

fn answers(repo: &Repo) -> Vec<String> {
    QUERIES
        .iter()
        .map(|q| {
            let r = repo.run(q);
            assert_eq!(r.code, 0, "{q:?}: {}", r.err);
            r.out
        })
        .collect()
}

fn expected() -> Vec<String> {
    let repo = repo_with_delta();
    answers(&repo)
}

fn le(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().unwrap())
}

/// 各区画の (開始, 長さ)。区画の長さはヘッダーの表から読む。
fn section_spans(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut start = HEADER_LEN;
    let spans = (0..SECTIONS)
        .map(|i| {
            let at = SECTION_TABLE_AT + i * SECTION_ENTRY_LEN;
            let len = usize::try_from(le(&bytes[at..at + 8])).unwrap();
            let span = (start, len);
            start += len;
            span
        })
        .collect();
    assert_eq!(start, bytes.len(), "HEADER_LEN must match the real header");
    spans
}

fn check(name: &str, target: fn(&(PathBuf, PathBuf)) -> &PathBuf, damage: impl Fn(&mut Vec<u8>)) {
    let repo = repo_with_delta();
    let files = index_files(&repo);
    let path = target(&files);
    let mut bytes = fs::read(path).unwrap();
    damage(&mut bytes);
    fs::write(path, bytes).unwrap();
    assert_eq!(answers(&repo), expected(), "{name}");
    let (main, _) = index_files(&repo);
    assert!(main.exists(), "{name}: rebuilt");
}

fn main_file(f: &(PathBuf, PathBuf)) -> &PathBuf {
    &f.0
}

fn delta_file(f: &(PathBuf, PathBuf)) -> &PathBuf {
    &f.1
}

#[test]
fn damaged_header_is_rebuilt() {
    for at in [0, 8, 12, 20, 50, HEADER_LEN - 1] {
        check(&format!("header byte {at}"), main_file, |b| b[at] ^= 0xff);
    }
}

#[test]
fn damaged_body_sections_are_rebuilt() {
    let repo = repo_with_delta();
    let main = fs::read(&index_files(&repo).0).unwrap();
    let spans = section_spans(&main);
    assert!(
        spans[5..].iter().all(|&(_, len)| len > 0),
        "reference sections must hold data so that damaging them is tested"
    );
    for (i, (start, len)) in spans.into_iter().enumerate() {
        if len == 0 {
            continue;
        }
        for (label, at) in [
            ("first", start),
            ("middle", start + len / 2),
            ("last", start + len - 1),
        ] {
            check(&format!("section {i} {label}"), main_file, |b| {
                b[at] ^= 0xff
            });
        }
    }
}

#[test]
fn damaged_delta_is_rebuilt() {
    check("delta byte", delta_file, |b| {
        let at = b.len() / 2;
        b[at] ^= 0xff;
    });
    check("delta header", delta_file, |b| b[3] ^= 0xff);
    check("delta truncated", delta_file, |b| b.truncate(b.len() / 2));
    check("delta zero-filled", delta_file, |b| b.fill(0));
    check("delta emptied", delta_file, Vec::clear);
}

#[test]
fn truncated_or_zeroed_main_is_rebuilt() {
    check("truncated to half", main_file, |b| b.truncate(b.len() / 2));
    check("truncated inside header", main_file, |b| {
        b.truncate(HEADER_LEN / 2)
    });
    check("one byte short", main_file, |b| {
        b.pop();
    });
    check("emptied", main_file, Vec::clear);
    check("zero-filled", main_file, |b| b.fill(0));
    check("body zero-filled", main_file, |b| b[HEADER_LEN..].fill(0));
}

#[test]
fn deleted_files_are_rebuilt() {
    let repo = repo_with_delta();
    let (main, delta) = index_files(&repo);
    fs::remove_file(&main).unwrap();
    assert_eq!(answers(&repo), expected());
    assert!(main.exists());
    let _ = delta;
}
