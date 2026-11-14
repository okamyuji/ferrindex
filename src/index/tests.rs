use std::fs;

use super::*;
use crate::intern::pack;
use crate::refs::Ref;

fn def(name: &str, line: u32, container: Option<&str>) -> Def {
    Def {
        name: name.into(),
        line,
        col: 3,
        kind: 2,
        container: container.map(Into::into),
        body: 0..0,
    }
}

fn entry(path: &str, defs: Vec<Def>) -> FileEntry {
    FileEntry {
        path: path.into(),
        mtime_ns: 1_700_000_000_123_456_789,
        size: 4242,
        lang: 7,
        defs,
        refs: Vec::new(),
    }
}

fn sample() -> Vec<FileEntry> {
    vec![
        FileEntry {
            refs: vec![reference("Alpha", 14, 1, Some("run"))],
            ..entry(
                "src/b.rs",
                vec![def("Alpha", 9, None), def("run", 12, Some("Alpha"))],
            )
        },
        entry(
            "src/a.rs",
            vec![def("Alpha", 1, None), def("zeta", 5, None)],
        ),
        FileEntry {
            lang: LANG_DELETED,
            ..entry("old.rs", vec![])
        },
    ]
}

fn written(files: &[FileEntry]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("index.fx");
    write_index(&p, files).unwrap();
    (dir, p)
}

fn open_bytes(bytes: &[u8]) -> Result<Index, OpenError> {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.fx");
    fs::write(&p, bytes).unwrap();
    Index::open(&p)
}

fn is_corrupt<T>(r: Result<T, OpenError>) -> bool {
    matches!(r, Err(OpenError::Corrupt))
}

#[test]
fn write_then_lookup() {
    let (_d, p) = written(&sample());
    let ix = Index::open(&p).unwrap();
    let hits = ix.lookup(b"Alpha").unwrap();
    let got: Vec<(&[u8], u32, u32)> = hits
        .iter()
        .map(|h| (h.path.as_slice(), h.row.line, h.row.col))
        .collect();
    assert_eq!(got, [(&b"src/a.rs"[..], 1, 3), (&b"src/b.rs"[..], 9, 3)]);
    assert_eq!(hits[0].row.kind, 2);
    assert_eq!(hits[0].row.container, 0);
    let run = ix.lookup(b"run").unwrap();
    assert_eq!(run.len(), 1);
    assert_eq!(
        ix.container_name(&run[0].row).as_deref(),
        Some(&b"Alpha"[..])
    );
    assert_eq!(ix.container_name(&hits[0].row), None);
    assert!(ix.lookup(b"missing").unwrap().is_empty());
    assert!(ix.lookup(b"Alph").unwrap().is_empty());
}

#[test]
fn container_without_own_rows_is_resolvable() {
    let files = [entry("a.rs", vec![def("m", 2, Some("OnlyContainer"))])];
    let (_d, p) = written(&files);
    let ix = Index::open(&p).unwrap();
    let h = &ix.lookup(b"m").unwrap()[0];
    assert_eq!(
        ix.container_name(&h.row).as_deref(),
        Some(&b"OnlyContainer"[..])
    );
    assert!(ix.lookup(b"OnlyContainer").unwrap().is_empty());
}

#[test]
fn file_id_is_byte_order_rank() {
    let (_d, p) = written(&sample());
    let ix = Index::open(&p).unwrap();
    let paths: Vec<Vec<u8>> = ix.files().map(|(k, _)| k).collect();
    assert_eq!(paths, [&b"old.rs"[..], b"src/a.rs", b"src/b.rs"]);
    let ids: Vec<u32> = ix
        .lookup(b"Alpha")
        .unwrap()
        .iter()
        .map(|h| h.row.file)
        .collect();
    assert_eq!(ids, [1, 2]);
    let stamps: Vec<Stamp> = ix.files().map(|(_, s)| s).collect();
    assert_eq!(stamps[0].lang, LANG_DELETED);
    assert_eq!(
        stamps[2],
        Stamp {
            mtime_ns: 1_700_000_000_123_456_789,
            size: 4242,
            lang: 7
        }
    );
}

#[test]
fn entries_restores_what_was_written() {
    let (_d, p) = written(&sample());
    let mut got = Index::open(&p).unwrap().entries().unwrap();
    for f in &mut got {
        f.defs.sort_by_key(|d| (d.line, d.col));
    }
    let mut want = sample();
    want.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(got, want);
}

#[test]
fn has_path_matches_exact_keys() {
    let (_d, p) = written(&sample());
    let ix = Index::open(&p).unwrap();
    assert!(ix.has_path(b"src/a.rs"));
    assert!(!ix.has_path(b"src/a"));
    assert!(!ix.has_path(b"src/c.rs"));
}

#[test]
fn stats_count_files_names_and_bytes() {
    let (_d, p) = written(&sample());
    let ix = Index::open(&p).unwrap();
    let s = ix.stats();
    assert_eq!((s.files, s.names), (3, 3));
    assert_eq!(s.bytes as u64, fs::metadata(&p).unwrap().len());
}

#[test]
fn section_bytes_plus_header_is_the_file_size() {
    let (_d, p) = written(&sample());
    let ix = Index::open(&p).unwrap();
    let parts = ix.section_bytes();
    let names: Vec<&str> = parts.iter().map(|p| p.0).collect();
    assert_eq!(
        names,
        [
            "paths",
            "names",
            "offsets",
            "postings",
            "stamps",
            "ref_offsets",
            "ref_postings"
        ]
    );
    assert!(parts.iter().all(|p| p.1 > 0));
    let sum: usize = parts.iter().map(|p| p.1).sum();
    assert_eq!(sum + HEADER_LEN, ix.stats().bytes);
}

#[test]
fn names_containing_ignores_ascii_case() {
    let (_d, p) = written(&sample());
    let ix = Index::open(&p).unwrap();
    let got = |needle: &str| ix.names_containing(needle.as_bytes()).collect::<Vec<_>>();
    assert_eq!(got("A"), [b"Alpha".to_vec(), b"zeta".to_vec()]);
    assert_eq!(got("RUN"), [b"run".to_vec()]);
    assert!(got("none").is_empty());
}

#[test]
fn contains_ignore_ascii_case_cases() {
    assert!(contains_ignore_ascii_case(b"OpenSesame", b"sesame"));
    assert!(contains_ignore_ascii_case(b"abc", b"abc"));
    assert!(!contains_ignore_ascii_case(b"ab", b"abc"));
    assert!(!contains_ignore_ascii_case(b"abc", b"abd"));
}

#[test]
fn empty_needle_matches_every_name() {
    assert!(contains_ignore_ascii_case(b"abc", b""));
    assert!(contains_ignore_ascii_case(b"", b""));
    let (_d, p) = written(&sample());
    let ix = Index::open(&p).unwrap();
    assert_eq!(
        ix.names_containing(b"").collect::<Vec<_>>(),
        [b"Alpha".to_vec(), b"run".to_vec(), b"zeta".to_vec()]
    );
}

#[test]
fn duplicate_paths_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let files = [entry("a", vec![]), entry("a", vec![])];
    let e = write_index(&dir.path().join("i.fx"), &files).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn rewrite_replaces_and_leaves_no_temp_file() {
    let (d, p) = written(&sample());
    write_index(&p, &[entry("only.rs", vec![def("solo", 1, None)])]).unwrap();
    let ix = Index::open(&p).unwrap();
    assert_eq!(ix.lookup(b"solo").unwrap().len(), 1);
    assert!(ix.lookup(b"Alpha").unwrap().is_empty());
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
}

#[test]
fn format_constants_and_helpers() {
    assert_eq!((HEADER_LEN, STAMP_LEN), (108, 17));
    assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert!(fingerprint() > 1);
    assert_eq!(to_u32(7).unwrap(), 7);
    assert_eq!(
        to_u32(u32::MAX as usize + 1).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(OpenError::Corrupt.to_string(), "index is corrupt");
    assert_eq!(
        OpenError::Fingerprint.to_string(),
        "index was built by a different extractor"
    );
    let io = OpenError::Io(io::Error::other("boom")).to_string();
    assert_eq!(io, "cannot read index: boom");
}

#[test]
fn missing_file_is_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let r = Index::open(&dir.path().join("none.fx"));
    assert!(matches!(r, Err(OpenError::Io(e)) if e.kind() == io::ErrorKind::NotFound));
}

#[test]
fn corrupt_section_detected() {
    let (_d, p) = written(&sample());
    let good = fs::read(&p).unwrap();
    let mut start = HEADER_LEN;
    for i in 0..SECTION_COUNT {
        let len = u64::from_le_bytes(good[20 + i * 12..28 + i * 12].try_into().unwrap()) as usize;
        assert!(len > 0, "section {i} is empty");
        let mut flipped = good.clone();
        flipped[start] ^= 1;
        assert!(is_corrupt(open_bytes(&flipped)), "bit flip in section {i}");
        let mut zeroed = good.clone();
        zeroed[start..start + len].fill(0);
        assert!(is_corrupt(open_bytes(&zeroed)), "zero fill in section {i}");
        start += len;
    }
    assert_eq!(start, good.len());
}

#[test]
fn every_single_bit_flip_is_detected() {
    let (_d, p) = written(&sample());
    let good = fs::read(&p).unwrap();
    for i in 0..good.len() {
        let mut b = good.clone();
        b[i] ^= 0x80;
        assert!(is_corrupt(open_bytes(&b)), "byte {i}");
    }
}

#[test]
fn truncation_and_trailing_bytes_are_detected() {
    let (_d, p) = written(&sample());
    let good = fs::read(&p).unwrap();
    for n in [0, 1, HEADER_LEN - 1, HEADER_LEN, good.len() - 1] {
        assert!(is_corrupt(open_bytes(&good[..n])), "len {n}");
    }
    let mut longer = good.clone();
    longer.push(0);
    assert!(is_corrupt(open_bytes(&longer)));
}

fn assembled(fp: u64, version: u32, s: &[Vec<u8>; 7]) -> Result<Index, OpenError> {
    open_bytes(&assemble(fp, version, s))
}

#[test]
fn fingerprint_mismatch_detected() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("i.fx");
    write_with(&p, &sample(), fingerprint() ^ 1).unwrap();
    assert!(matches!(Index::open(&p), Err(OpenError::Fingerprint)));
    let s = sections(&sample()).unwrap();
    assert!(matches!(
        assembled(fingerprint(), FORMAT_VERSION + 1, &s),
        Err(OpenError::Fingerprint)
    ));
    assert!(assembled(fingerprint(), FORMAT_VERSION, &s).is_ok());
}

#[test]
fn bad_magic_with_valid_crc_is_corrupt() {
    let s = sections(&sample()).unwrap();
    let mut b = assemble(fingerprint(), FORMAT_VERSION, &s);
    b[0] ^= 1;
    let crc = crc32c::crc32c(&b[..HEADER_LEN - 4]);
    b[HEADER_LEN - 4..HEADER_LEN].copy_from_slice(&crc.to_le_bytes());
    assert!(is_corrupt(open_bytes(&b)));
}

#[test]
fn inconsistent_sections_are_corrupt() {
    let good = sections(&[entry("a", vec![def("x", 1, None)]), entry("b", vec![])]).unwrap();
    assert!(assembled(fingerprint(), FORMAT_VERSION, &good).is_ok());

    let mut s = good.clone();
    s[4].extend_from_slice(&[0; 17]);
    assert!(
        is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
        "stamps too long"
    );
    let mut s = good.clone();
    s[4].pop();
    assert!(
        is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
        "stamps too short"
    );
    let mut s = good.clone();
    s[2].extend_from_slice(&[0; 8]);
    assert!(
        is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
        "offsets too long"
    );
    let mut s = good.clone();
    s[2].truncate(8);
    assert!(
        is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
        "offsets too short"
    );
    let mut s = good.clone();
    s[0] = vec![1, 2, 3];
    assert!(
        is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
        "path fst garbage"
    );
    let mut s = good.clone();
    s[1] = vec![1, 2, 3];
    assert!(
        is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
        "name fst garbage"
    );
}

#[test]
fn lookup_reports_corrupt_postings() {
    let good = sections(&[entry("a", vec![def("x", 1, None)])]).unwrap();
    let end = u64::try_from(good[3].len()).unwrap();
    let lookup = |s: &[Vec<u8>; 7]| {
        assembled(fingerprint(), FORMAT_VERSION, s)
            .unwrap()
            .lookup(b"x")
    };
    assert_eq!(lookup(&good).unwrap().len(), 1);

    let mut s = good.clone();
    s[2] = [0u64, end + 1]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert!(lookup(&s).is_err(), "end past postings");
    let mut s = good.clone();
    s[2] = [end, 0u64].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert!(lookup(&s).is_err(), "start after end");
    let mut s = good.clone();
    s[3] = vec![0xff; good[3].len()];
    assert!(lookup(&s).is_err(), "garbage postings");
    let mut s = good.clone();
    s[3] = vec![1, 5, 0, 0, 0];
    s[2] = [0u64, 5].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert!(lookup(&s).is_err(), "file id out of range");
    let mut s = good.clone();
    s[3] = vec![1, 0, 0, 0, 9];
    s[2] = [0u64, 5].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert!(lookup(&s).is_err(), "container out of range");
}

fn reference(name: &str, line: u32, kind: u8, container: Option<&str>) -> PackedRef {
    pack(&[Ref {
        name: name.into(),
        line,
        col: 4,
        kind,
        container: container.map(Into::into),
    }])[0]
}

#[test]
fn refs_roundtrip() {
    let files = [
        FileEntry {
            refs: vec![
                reference("Alpha", 20, 1, Some("run")),
                reference("Alpha", 3, 2, None),
            ],
            ..entry(
                "src/a.rs",
                vec![def("Alpha", 1, None), def("run", 10, None)],
            )
        },
        FileEntry {
            refs: vec![reference("Alpha", 7, 0, None)],
            ..entry("src/b.rs", vec![])
        },
    ];
    let (_d, p) = written(&files);
    let ix = Index::open(&p).unwrap();
    let hits = ix.lookup_refs(b"Alpha").unwrap();
    let got: Vec<_> = hits
        .iter()
        .map(|h| {
            (
                h.path.clone(),
                h.row.line,
                h.row.col,
                h.row.kind,
                ix.container_name(&h.row),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (b"src/a.rs".to_vec(), 3, 4, 2, None),
            (b"src/a.rs".to_vec(), 20, 4, 1, Some(b"run".to_vec())),
            (b"src/b.rs".to_vec(), 7, 4, 0, None),
        ]
    );
    // 定義の posting に参照が混ざらない
    assert_eq!(ix.lookup(b"Alpha").unwrap().len(), 1);
    assert!(ix.lookup_refs(b"zeta").unwrap().is_empty());
}

#[test]
fn ref_only_names_are_in_the_name_fst() {
    let files = [FileEntry {
        refs: vec![reference("external_fn", 5, 1, Some("caller"))],
        ..entry("a.rs", vec![])
    }];
    let (_d, p) = written(&files);
    let ix = Index::open(&p).unwrap();
    assert_eq!(ix.lookup_refs(b"external_fn").unwrap().len(), 1);
    assert!(ix.lookup(b"external_fn").unwrap().is_empty());
    let hit = &ix.lookup_refs(b"external_fn").unwrap()[0];
    assert_eq!(ix.container_name(&hit.row), Some(b"caller".to_vec()));
}

#[test]
fn entries_restore_refs() {
    let files = [FileEntry {
        refs: vec![
            reference("Alpha", 20, 1, Some("run")),
            reference("beta", 2, 0, None),
        ],
        ..entry("a.rs", vec![def("Alpha", 1, None), def("run", 10, None)])
    }];
    let (_d, p) = written(&files);
    let got = Index::open(&p).unwrap().entries().unwrap();
    let mut refs = got[0].refs.clone();
    refs.sort_by_key(|r| r.line);
    assert_eq!(
        refs,
        files[0].refs.iter().rev().cloned().collect::<Vec<_>>()
    );
}

#[test]
fn corrupt_ref_postings_are_reported() {
    let good = sections(&[FileEntry {
        refs: vec![reference("x", 1, 0, None)],
        ..entry("a", vec![])
    }])
    .unwrap();
    let lookup = |s: &[Vec<u8>; 7]| {
        assembled(fingerprint(), FORMAT_VERSION, s)
            .unwrap()
            .lookup_refs(b"x")
    };
    assert_eq!(lookup(&good).unwrap().len(), 1);
    let mut s = good.clone();
    s[6] = vec![0xff; good[6].len()];
    assert!(lookup(&s).is_err(), "garbage ref postings");
    let mut s = good.clone();
    s[5].extend_from_slice(&[0; 8]);
    assert!(
        is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
        "ref offsets too long"
    );
    let mut s = good.clone();
    s[5] = [0u64, u64::try_from(good[6].len()).unwrap() + 1]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert!(lookup(&s).is_err(), "ref end past postings");
}
