mod common;

use std::{fs, os::unix::fs::PermissionsExt};

use common::Repo;

/// PATH に rust-analyzer だけを置き、HOME も試験の一時ディレクトリにして実行する。
fn servers(r: &Repo) -> common::Run {
    let bin = r.xdg.with_file_name("bin");
    fs::create_dir_all(&bin).unwrap();
    let ra = bin.join("rust-analyzer");
    fs::write(&ra, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&ra, fs::Permissions::from_mode(0o755)).unwrap();
    let home = r.xdg.with_file_name("home");
    r.run_with(
        &[
            ("PATH", bin.to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
        ],
        &["servers"],
    )
}

#[test]
fn servers_lists_each_indexed_language_with_its_server() {
    let r = Repo::new();
    r.write("a.rs", "fn a() {}\n");
    r.write("b.rs", "fn b() {}\n");
    r.write("g.go", "package g\n");
    r.write("c.c", "int c(void){return 0;}\n");
    r.write("README.md", "# x\n");
    let o = servers(&r);
    assert_eq!(o.code, 0, "{}", o.err);
    assert_eq!(
        o.lines(),
        [
            "language\tfiles\tserver\tinstalled\tprerequisite\tinstall",
            "rust\t2\trust-analyzer\tyes\tok\trustup component add rust-analyzer",
            "go\t1\tgopls\tno\tok\tgo install golang.org/x/tools/gopls@latest",
            "c\t1\tclangd\tno\tcompile_commands.json\tbrew install llvm",
        ]
    );
}

#[test]
fn compile_commands_json_satisfies_clangd() {
    let r = Repo::new();
    r.write("c.c", "int c(void){return 0;}\n");
    r.write("compile_commands.json", "[]\n");
    let o = servers(&r);
    assert_eq!(o.lines()[1], "c\t1\tclangd\tno\tok\tbrew install llvm");
}

#[test]
fn servers_without_code_files_exits_1() {
    let r = Repo::new();
    r.write("README.md", "# x\n");
    let o = servers(&r);
    assert_eq!(o.code, 1, "{}", o.err);
    assert_eq!(
        o.lines(),
        ["language\tfiles\tserver\tinstalled\tprerequisite\tinstall"]
    );
}

#[test]
fn servers_needs_a_git_repository_and_no_arguments() {
    let r = Repo::new();
    let o = common::run_at(r.root.parent().unwrap(), &r.xdg, &["servers"]);
    assert_eq!(o.code, 3, "{}", o.err);
    assert_eq!(o.err, "ferrindex: not inside a git repository\n");
    let o = r.run(&["servers", "x"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("wrong number of arguments"), "{}", o.err);
    assert_eq!(r.run(&["servers", "--limit", "1"]).code, 2);
}

#[test]
fn java_and_dotnet_prerequisites_follow_the_environment() {
    let r = Repo::new();
    r.write("A.java", "class A {}\n");
    r.write("B.cs", "class B {}\n");
    let jdk = r.xdg.with_file_name("jdk");
    fs::create_dir_all(&jdk).unwrap();
    fs::write(jdk.join("release"), "JAVA_VERSION=\"25\"\n").unwrap();
    let dotnet = r.xdg.with_file_name("dotnet");
    fs::create_dir_all(&dotnet).unwrap();
    let o = r.run_with(
        &[
            ("FERRINDEX_JAVA_HOME", jdk.to_str().unwrap()),
            ("DOTNET_ROOT", dotnet.to_str().unwrap()),
        ],
        &["servers"],
    );
    let prereq: Vec<&str> = o.lines()[1..]
        .iter()
        .map(|l| l.split('\t').nth(4).unwrap())
        .collect();
    assert_eq!(prereq, ["ok", "ok"], "{}", o.out);
}
