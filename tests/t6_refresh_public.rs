use std::fs;

use ferrindex::fresh::refresh;

#[test]
fn refresh_writes_under_xdg_cache_home_and_finds_definitions() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("a.rs"), "fn public_entry() {}\n").unwrap();
    let cache = t.path().join("xdg");
    // SAFETY: このバイナリのテストはこの 1 件だけで、他のスレッドが環境変数を読まない。
    unsafe { std::env::set_var("XDG_CACHE_HOME", &cache) };
    let view = refresh(&fs::canonicalize(&root).unwrap()).unwrap();
    let hits = view.lookup(b"public_entry").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, b"a.rs");
    assert!(cache.join("ferrindex").is_dir());
}
