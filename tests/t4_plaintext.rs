use ferrindex::extract::Def;
use ferrindex::index::{FileEntry, Index, write_index};

const NAME: &str = "zq_UniquelyLongIdentifierForPlaintextCheck";
const CONTAINER: &str = "ZqEnclosingTypeWithAnotherLongUniqueName";
const PATH: &str = "crates/zq_deeply/nested_directory/unique_module_file_name.rs";

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn names_and_paths_do_not_appear_in_order_in_the_file() {
    let defs = vec![Def {
        name: NAME.into(),
        line: 10,
        col: 5,
        kind: 1,
        container: Some(CONTAINER.into()),
        body: 0..0,
    }];
    let files = [FileEntry {
        path: PATH.into(),
        mtime_ns: 1,
        size: 2,
        lang: 3,
        defs,
    }];
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("index.fx");
    write_index(&p, &files).unwrap();
    let bytes = std::fs::read(&p).unwrap();

    // `strings -n 8` が拾う長さの連続列も含めて、正順では現れない
    for s in [NAME, CONTAINER, PATH] {
        for w in s.as_bytes().windows(8) {
            assert!(
                !contains(&bytes, w),
                "found {:?}",
                String::from_utf8_lossy(w)
            );
        }
    }
    let ix = Index::open(&p).unwrap();
    assert_eq!(ix.lookup(NAME.as_bytes()).unwrap()[0].path, PATH.as_bytes());
}
