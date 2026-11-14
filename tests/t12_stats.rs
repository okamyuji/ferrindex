mod common;
use common::{Repo, SAMPLE};

fn fidx_bytes(dir: &std::path::Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(fidx_bytes(&p));
        } else if p.extension().is_some_and(|x| x == "fidx") {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            out.push((name, std::fs::metadata(&p).unwrap().len()));
        }
    }
    out.sort();
    out
}

#[test]
fn stats_report_size_and_section_breakdown() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    r.write("b.rs", "fn other() {}\n");
    let run = r.run(&["update", "--stats"]);
    assert_eq!(run.code, 0, "{}", run.err);
    let files = fidx_bytes(&r.xdg);
    assert_eq!(files.len(), 1, "{files:?}");
    let size = files[0].1;
    let lines = run.lines();
    assert_eq!(lines[0], format!("total {size} bytes"));
    assert!(
        lines[1].starts_with("main: 2 files, 5 names, "),
        "{lines:?}"
    );
    assert!(lines[1].ends_with(&format!(" {size} bytes")), "{lines:?}");
    let parts: Vec<(&str, u64)> = lines[2..]
        .iter()
        .map(|l| {
            let (k, v) = l.trim().split_once(' ').unwrap();
            (k, v.strip_suffix(" bytes").unwrap().parse().unwrap())
        })
        .collect();
    let names: Vec<&str> = parts.iter().map(|p| p.0).collect();
    assert_eq!(
        names,
        ["header", "paths", "names", "offsets", "postings", "stamps"]
    );
    assert!(parts.iter().all(|p| p.1 > 0), "{parts:?}");
    assert_eq!(parts.iter().map(|p| p.1).sum::<u64>(), size);
}

#[test]
fn stats_show_the_delta_part_after_an_edit() {
    let r = Repo::new();
    for i in 0..30 {
        r.write(&format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    r.age();
    assert_eq!(r.run(&["update"]).code, 0);
    r.write("f0.rs", "fn changed() {}\n");
    let run = r.run(&["update", "--stats"]);
    let lines = run.lines();
    assert!(lines[0].starts_with("total "), "{lines:?}");
    let total: u64 = lines[0].split(' ').nth(1).unwrap().parse().unwrap();
    assert_eq!(
        total,
        fidx_bytes(&r.xdg).iter().map(|f| f.1).sum::<u64>(),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("main: 30 files")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("delta: 1 files")),
        "{lines:?}"
    );
}

#[test]
fn plain_update_reports_file_count() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    let run = r.run(&["update"]);
    assert_eq!((run.code, run.out.as_str()), (0, "indexed 1 files\n"));
}

#[test]
fn rebuild_replaces_main_and_drops_delta() {
    let r = Repo::new();
    for i in 0..30 {
        r.write(&format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    r.age();
    r.run(&["update"]);
    r.write("f0.rs", "fn changed() {}\n");
    r.run(&["update"]);
    assert_eq!(fidx_bytes(&r.xdg).len(), 2);
    let run = r.run(&["update", "--rebuild"]);
    assert_eq!(run.code, 0, "{}", run.err);
    let files = fidx_bytes(&r.xdg);
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(
        r.run(&["find", "changed"]).lines(),
        ["f0.rs:1:4 function changed"]
    );
}
