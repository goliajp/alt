//! Git LFS: commits keep pointer files, so ids match git+LFS repositories,
//! while the working tree holds the real contents.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use alt_lfs::Pointer;

const ATTRS: &str = "*.bin filter=lfs diff=lfs merge=lfs -text\n";

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("ALT_NO_CREDENTIAL_HELPER", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap();
    ok(o)
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn clean(root: &Path) -> bool {
    ok(alt(root, &["status"])).contains("working tree clean")
}

/// A git repository as git-lfs leaves it: pointers committed, contents in
/// `.git/lfs/objects`. Returns the content of `model.bin`.
fn git_lfs_repo(src: &Path) -> Vec<u8> {
    let content: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let p = Pointer::of(&content);
    git(src, &["init", "-q", "-b", "main"]);
    fs::write(src.join(".gitattributes"), ATTRS).unwrap();
    fs::write(src.join("model.bin"), p.encode()).unwrap();
    fs::write(src.join("readme.txt"), "hi\n").unwrap();
    git(src, &["add", "."]);
    git(src, &["commit", "-q", "-m", "add model"]);
    let dir = src
        .join(".git/lfs/objects")
        .join(&p.oid[..2])
        .join(&p.oid[2..4]);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(&p.oid), &content).unwrap();
    content
}

#[test]
fn import_checks_out_contents_and_export_keeps_the_pointers() {
    let (s, d, e) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let content = git_lfs_repo(s.path());
    ok(alt(s.path(), &["import", &d.path().display().to_string()]));
    let dst = d.path();
    assert_eq!(fs::read(dst.join("model.bin")).unwrap(), content);
    assert!(clean(dst));
    assert!(ok(alt(dst, &["lfs", "ls"])).contains("* model.bin"));

    // editing the file is a change; putting it back is not
    fs::write(dst.join("model.bin"), b"other").unwrap();
    assert!(!clean(dst));
    fs::write(dst.join("model.bin"), &content).unwrap();
    assert!(clean(dst));

    ok(alt(dst, &["export", &e.path().display().to_string()]));
    assert_eq!(
        git(e.path(), &["rev-parse", "main"]),
        git(s.path(), &["rev-parse", "main"])
    );
    // and the contents stay out of git: the same objects, nothing more
    let objects = |r: &Path| git(r, &["cat-file", "--batch-all-objects", "--batch-check"]);
    assert_eq!(objects(e.path()), objects(s.path()));
}

#[test]
fn contents_arrive_later_from_the_source_repository() {
    let (s, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let content = git_lfs_repo(s.path());
    let lfs = s.path().join(".git/lfs");
    fs::rename(&lfs, s.path().join("lfs-aside")).unwrap();
    ok(alt(s.path(), &["import", &d.path().display().to_string()]));
    let dst = d.path();
    // only the pointer so far, which is what HEAD holds: clean
    assert!(Pointer::parse(&fs::read(dst.join("model.bin")).unwrap()).is_some());
    assert!(ok(alt(dst, &["lfs", "ls"])).contains("- model.bin"));
    assert!(clean(dst));

    fs::rename(s.path().join("lfs-aside"), &lfs).unwrap();
    ok(alt(
        dst,
        &[
            "lfs",
            "import",
            &s.path().join(".git").display().to_string(),
        ],
    ));
    assert_eq!(fs::read(dst.join("model.bin")).unwrap(), content);
    assert!(clean(dst));
}

#[test]
fn adding_an_lfs_path_commits_the_pointer_git_lfs_would() {
    let (a, g) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let content = vec![9u8; 50_000];
    ok(alt(a.path(), &["init", "."]));
    fs::write(a.path().join(".gitattributes"), ATTRS).unwrap();
    fs::write(a.path().join("w.bin"), &content).unwrap();
    fs::write(a.path().join("small.txt"), "plain\n").unwrap();
    ok(alt(a.path(), &["add", "."]));
    ok(alt(a.path(), &["commit", "-m", "weights"]));
    assert!(clean(a.path()));

    // git with git-lfs installed commits the pointer in the file's place
    git(g.path(), &["init", "-q", "-b", "main"]);
    fs::write(g.path().join(".gitattributes"), ATTRS).unwrap();
    fs::write(g.path().join("w.bin"), Pointer::of(&content).encode()).unwrap();
    fs::write(g.path().join("small.txt"), "plain\n").unwrap();
    git(g.path(), &["add", "."]);
    git(g.path(), &["commit", "-q", "-m", "weights"]);
    let tree = |cat: String| cat.lines().next().unwrap().to_owned();
    assert_eq!(
        tree(ok(alt(a.path(), &["cat-file", "-p", "main"]))),
        tree(git(g.path(), &["cat-file", "-p", "main"]))
    );

    // switching away and back writes the content, not the pointer
    ok(alt(a.path(), &["switch", "-c", "side"]));
    fs::remove_file(a.path().join("w.bin")).unwrap();
    ok(alt(a.path(), &["add", "."]));
    ok(alt(a.path(), &["commit", "-m", "drop"]));
    ok(alt(a.path(), &["switch", "main"]));
    assert_eq!(fs::read(a.path().join("w.bin")).unwrap(), content);
    assert!(clean(a.path()));
}

/// Serves `objects` through a minimal batch API; returns the repo URL.
fn serve(objects: HashMap<String, Vec<u8>>) -> String {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let base = format!("http://{}", server.server_addr());
    let repo = format!("{base}/team/repo");
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let url = req.url().to_owned();
            if url == "/team/repo.git/info/lfs/objects/batch" {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
                let asked: serde_json::Value = serde_json::from_str(&body).unwrap();
                let answers: Vec<serde_json::Value> = asked["objects"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|o| {
                        let oid = o["oid"].as_str().unwrap();
                        serde_json::json!({"oid": oid, "size": o["size"],
                            "actions": {"download": {"href": format!("{base}/content/{oid}")}}})
                    })
                    .collect();
                let resp = serde_json::json!({"objects": answers});
                req.respond(tiny_http::Response::from_string(resp.to_string()))
                    .unwrap();
            } else if let Some(oid) = url.strip_prefix("/content/") {
                let bytes = objects[oid].clone();
                req.respond(tiny_http::Response::from_data(bytes)).unwrap();
            } else {
                req.respond(tiny_http::Response::empty(404)).unwrap();
            }
        }
    });
    repo
}

#[test]
fn fetch_downloads_from_the_remotes_lfs_server() {
    let (s, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let content = git_lfs_repo(s.path());
    fs::remove_dir_all(s.path().join(".git/lfs")).unwrap();
    ok(alt(s.path(), &["import", &d.path().display().to_string()]));
    let dst = d.path();
    let url = serve(HashMap::from([(
        Pointer::of(&content).oid,
        content.clone(),
    )]));
    ok(alt(dst, &["remote", "add", "origin", &url]));
    let out = ok(alt(dst, &["lfs", "fetch"]));
    assert!(out.contains("fetched 1 LFS object(s); 1 file(s)"), "{out}");
    assert_eq!(fs::read(dst.join("model.bin")).unwrap(), content);
    assert!(clean(dst));
    // nothing left to fetch
    assert!(ok(alt(dst, &["lfs", "fetch"])).contains("fetched 0"));
}
