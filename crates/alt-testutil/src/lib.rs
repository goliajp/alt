//! Internal test fixtures: builds real git repositories exercising every
//! object kind and tree-entry mode, and walks their objects.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;

use alt_git_codec::{LooseStore, ObjectId, RawObject};

pub fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=alt",
            "-c",
            "user.email=alt@test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git must be runnable");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Two branches with a merge, an annotated tag, a gitlink, a symlink, an
/// executable, and awkward file names — all objects left loose (no gc).
pub fn make_repo(dir: &Path, object_format: &str) {
    make_repo_opts(dir, object_format, "files");
}

/// [`make_repo`] with an explicit ref storage format (`files`/`reftable`).
pub fn make_repo_opts(dir: &Path, object_format: &str, ref_format: &str) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .args(["--object-format", object_format])
        .args(["--ref-format", ref_format])
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    fs::write(dir.join("a.txt"), "hello world\n").unwrap();
    // large enough that repacking will delta its two versions
    let big: String = (0..200)
        .map(|i| format!("line {i} of some text\n"))
        .collect();
    fs::write(dir.join("big.txt"), &big).unwrap();
    fs::write(dir.join("b.bin"), [0u8, 159, 146, 150]).unwrap();
    fs::create_dir(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/c.txt"), "nested\n").unwrap();
    fs::write(dir.join("exec.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(dir.join("exec.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("a.txt", dir.join("link")).unwrap();
    fs::write(dir.join("has \"quotes\".txt"), "q\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "first"]);

    // Gitlink entry; the target id just has to be well-formed, so reuse HEAD.
    let head = git(dir, &["rev-parse", "HEAD"]);
    git(
        dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},submod", head.trim()),
        ],
    );
    git(dir, &["commit", "-q", "-m", "add gitlink"]);

    git(dir, &["checkout", "-q", "-b", "feat"]);
    fs::write(dir.join("f.txt"), "feature\n").unwrap();
    git(dir, &["add", "f.txt"]);
    git(dir, &["commit", "-q", "-m", "feat work"]);
    git(dir, &["checkout", "-q", "main"]);
    fs::write(dir.join("a.txt"), "hello again\n").unwrap();
    let big: String = (0..201)
        .map(|i| format!("line {i} of some text\n"))
        .collect();
    fs::write(dir.join("big.txt"), &big).unwrap();
    git(dir, &["commit", "-q", "-am", "second"]);
    git(dir, &["merge", "-q", "--no-ff", "-m", "merge feat", "feat"]);

    git(
        dir,
        &["tag", "-a", "v0", "-m", "annotated tag\n\nwith body"],
    );
}

/// Repacks everything into a single packfile (deltas allowed) and drops the
/// loose copies; returns the `.git/objects/pack` directory.
pub fn pack_repo(dir: &Path) -> std::path::PathBuf {
    git(dir, &["repack", "-adq"]);
    dir.join(".git/objects/pack")
}

/// Calls `f` with every loose object in the repository; returns the count.
pub fn for_each_loose(repo: &Path, mut f: impl FnMut(ObjectId, RawObject)) -> usize {
    let objects = repo.join(".git/objects");
    let store = LooseStore::new(&objects);
    let mut count = 0;
    for entry in fs::read_dir(&objects).unwrap() {
        let entry = entry.unwrap();
        let fanout = entry.file_name().into_string().unwrap();
        if fanout.len() != 2 {
            continue; // info/, pack/
        }
        for obj in fs::read_dir(entry.path()).unwrap() {
            let rest = obj.unwrap().file_name().into_string().unwrap();
            let oid = ObjectId::from_hex(format!("{fanout}{rest}").as_bytes()).unwrap();
            f(oid, store.read(&oid).unwrap());
            count += 1;
        }
    }
    count
}
