//! `scan_worktree` honours `.gitignore`. A working-tree fixture seeds a
//! mix of ignored and tracked content; the scan must include only the
//! tracked entries.

use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_worktree::scan_worktree;
use bstr::ByteSlice;

fn setup(root: &Path) {
    // root-level .gitignore — the shape `alt`'s own project uses
    std::fs::write(root.join(".gitignore"), "/.dev/\n/.alt/\n/target/\n*.log\n").unwrap();

    // tracked content
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "// hi\n").unwrap();

    // ignored: each shape that the fixture .gitignore covers
    std::fs::create_dir(root.join(".dev")).unwrap();
    std::fs::write(root.join(".dev/corpus.tar"), [0xff; 1024]).unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    std::fs::write(root.join("target/binary"), [0xab; 2048]).unwrap();
    std::fs::write(root.join("run.log"), "noise\n").unwrap();
    std::fs::create_dir(root.join("src/inner")).unwrap();
    std::fs::write(root.join("src/inner/debug.log"), "more noise\n").unwrap();
}

#[test]
fn scan_worktree_skips_gitignored_paths() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    let entries = scan_worktree(dir.path(), HashAlgo::Sha1).unwrap();
    let paths: Vec<String> = entries
        .iter()
        .map(|e| e.path.to_str_lossy().into_owned())
        .collect();

    // tracked content is present
    assert!(paths.contains(&".gitignore".to_string()), "{paths:?}");
    assert!(paths.contains(&"Cargo.toml".to_string()), "{paths:?}");
    assert!(paths.contains(&"src/lib.rs".to_string()), "{paths:?}");

    // ignored content is absent
    for p in &paths {
        assert!(
            !p.starts_with(".dev/"),
            ".dev/ should not have been scanned: {p:?}"
        );
        assert!(
            !p.starts_with("target/"),
            "target/ should not have been scanned: {p:?}"
        );
        assert!(!p.ends_with(".log"), "*.log should be ignored: {p:?}");
    }
}

#[test]
fn scan_worktree_respects_nested_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    // root keeps it simple; the nested .gitignore adds a rule scoped to its subtree
    std::fs::write(dir.path().join(".gitignore"), "").unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::create_dir(dir.path().join("crate")).unwrap();
    std::fs::write(dir.path().join("crate/.gitignore"), "target/\n*.tmp\n").unwrap();
    std::fs::write(dir.path().join("crate/keep.rs"), "// keep\n").unwrap();
    std::fs::write(dir.path().join("crate/scratch.tmp"), "ignored\n").unwrap();
    std::fs::create_dir(dir.path().join("crate/target")).unwrap();
    std::fs::write(dir.path().join("crate/target/build"), b"output\n").unwrap();

    let entries = scan_worktree(dir.path(), HashAlgo::Sha1).unwrap();
    let paths: Vec<String> = entries
        .iter()
        .map(|e| e.path.to_str_lossy().into_owned())
        .collect();

    assert!(paths.contains(&"crate/keep.rs".to_string()), "{paths:?}");
    assert!(paths.contains(&"crate/.gitignore".to_string()), "{paths:?}");
    assert!(
        !paths.iter().any(|p| p.starts_with("crate/target/")),
        "nested target/ rule must apply: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "crate/scratch.tmp"),
        "nested *.tmp rule must apply: {paths:?}"
    );
}
