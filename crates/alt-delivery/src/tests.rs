use super::*;
use std::{path::PathBuf, process::Command};
fn git_path() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .unwrap()
        .canonicalize()
        .unwrap()
}
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new(git_path())
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@test")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn temp() -> alt_os::temp::TempDir {
    alt_os::temp::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap()
}
fn source(root: &Path, algo: &str, backend: &str) -> PathBuf {
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    git(
        &source,
        &[
            "init",
            "-b",
            "main",
            &format!("--object-format={algo}"),
            &format!("--ref-format={backend}"),
        ],
    );
    fs::write(source.join("readme.txt"), b"canonical\0bytes\xff\n").unwrap();
    fs::write(source.join("gate.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(source.join("gate.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("readme.txt", source.join("relative-link")).unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "base"]);
    git(&source, &["tag", "-a", "v1", "-m", "annotated"]);
    git(&source, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&source, &["notes", "add", "-m", "public note"]);
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://example.invalid/repo.git",
        ],
    );
    git(
        &source,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    fs::write(root.join("orphan"), "original unreachable blob").unwrap();
    git(
        &source,
        &["hash-object", "-w", root.join("orphan").to_str().unwrap()],
    );
    git(&source, &["pack-refs", "--all"]);
    source
}
#[test]
fn complete_independent_roundtrip_preserves_all_original_objects_and_excludes_later_private_objects()
 {
    for (algo, backend) in [("sha1", "files"), ("sha256", "files"), ("sha1", "reftable")] {
        let temp = temp();
        let source = source(temp.path(), algo, backend);
        let before = git(&source, &["show-ref"]);
        let native = temp.path().join("native");
        let intake = intake(&source, &native, &git_path()).unwrap();
        assert!(!native.join(".git").exists());
        let mut odb = NativeOdb::open(native.join(".alt")).unwrap();
        let private = ObjectId::hash_object(
            intake.baseline.algo().unwrap(),
            ObjectKind::Blob,
            b"later unreviewed",
        );
        odb.put(private, ObjectKind::Blob, b"later unreviewed")
            .unwrap();
        odb.flush().unwrap();
        let output = temp.path().join("output");
        let report = export(
            &odb,
            &intake.baseline,
            intake.baseline.refs.clone(),
            &[],
            &output,
            &git_path(),
        )
        .unwrap();
        assert_eq!(report.exported.objects, intake.baseline.objects);
        assert!(!report.exported.objects.contains_key(&private.to_string()));
        assert_eq!(git(&output, &["show-ref"]), before);
        assert_eq!(git(&source, &["show-ref"]), before);
        assert_eq!(
            fs::read(output.join("readme.txt")).unwrap(),
            b"canonical\0bytes\xff\n"
        );
        assert!(!output.join(".alt").exists());
        assert!(git(&output, &["status", "--porcelain"]).is_empty());
        let all = git(
            &output,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ],
        );
        let ids: BTreeSet<_> = std::str::from_utf8(&all)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(ids, report.exported.objects.keys().cloned().collect());
        assert!(
            export(
                &odb,
                &intake.baseline,
                intake.baseline.refs.clone(),
                &[],
                &output,
                &git_path()
            )
            .is_err()
        );
    }
}
#[test]
fn unsupported_git_inputs_are_rejected_before_destination_publication() {
    for form in [
        "shallow",
        "alternates",
        "promisor",
        "sparse",
        "hook",
        "include",
        "filter",
        "credentials",
        "linked",
        "coexist",
    ] {
        let temp = temp();
        let source = source(temp.path(), "sha1", "files");
        let dir = source.join(".git");
        match form {
            "shallow" => fs::write(dir.join("shallow"), "missing").unwrap(),
            "alternates" => fs::write(dir.join("objects/info/alternates"), "/unavailable").unwrap(),
            "promisor" => fs::write(dir.join("objects/pack/fixture.promisor"), "").unwrap(),
            "sparse" => fs::write(dir.join("info/sparse-checkout"), "readme.txt").unwrap(),
            "hook" => fs::write(dir.join("hooks/pre-commit"), "exit 99").unwrap(),
            "include" => git(&source, &["config", "include.path", "/unavailable"]).clear(),
            "filter" => git(
                &source,
                &["config", "filter.custom.clean", "arbitrary-command"],
            )
            .clear(),
            "credentials" => git(
                &source,
                &[
                    "config",
                    "remote.origin.url",
                    "https://user:secret@example.invalid/repo",
                ],
            )
            .clear(),
            "linked" => fs::write(dir.join("commondir"), "/unavailable").unwrap(),
            "coexist" => fs::create_dir(source.join(".alt")).unwrap(),
            _ => unreachable!(),
        }
        let output = temp.path().join("output");
        let error = intake(&source, &output, &git_path())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret"));
        assert!(!output.exists(), "{form}");
    }
}
#[test]
fn destination_filesystem_detects_name_aliases_and_rejects_escaping_links() {
    let temp = temp();
    let mut odb = NativeOdb::open(temp.path().join("odb")).unwrap();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"x");
    odb.put(oid, ObjectKind::Blob, b"x").unwrap();
    odb.flush().unwrap();
    for pair in [["Name/file", "name/file"], ["é", "e\u{301}"]] {
        let entries = pair
            .into_iter()
            .map(|p| {
                (
                    p.into(),
                    FileEntry {
                        mode: 0o100644,
                        oid: oid.to_string(),
                    },
                )
            })
            .collect();
        let result = preflight_paths(temp.path(), &entries, &odb);
        #[cfg(target_os = "macos")]
        assert!(result.is_err());
        #[cfg(target_os = "linux")]
        result.unwrap();
    }
    let bad = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"../outside");
    odb.put(bad, ObjectKind::Blob, b"../outside").unwrap();
    odb.flush().unwrap();
    assert!(
        preflight_paths(
            temp.path(),
            &BTreeMap::from([(
                "link".into(),
                FileEntry {
                    mode: 0o120000,
                    oid: bad.to_string()
                }
            )]),
            &odb
        )
        .is_err()
    );
    let destination = temp.path().join("exists");
    fs::create_dir(&destination).unwrap();
    let staging = temp.path().join("stage");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("payload"), "ours").unwrap();
    assert!(publish_directory(&staging, &destination).is_err());
    assert!(staging.join("payload").exists());
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
}

#[test]
fn execution_intake_rejects_gitlinks_lfs_and_non_utf8_tree_names() {
    for form in ["gitlink", "lfs", "non-utf8"] {
        let temp = temp();
        let source = source(temp.path(), "sha1", "files");
        match form {
            "gitlink" => {
                let head = String::from_utf8(git(&source, &["rev-parse", "HEAD"])).unwrap();
                git(
                    &source,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        "160000",
                        head.trim(),
                        "submodule",
                    ],
                );
            }
            "lfs" => {
                fs::write(source.join("pointer"), b"version https://git-lfs.github.com/spec/v1\noid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize 123\n").unwrap();
                git(&source, &["add", "pointer"]);
            }
            "non-utf8" => {
                use std::os::unix::ffi::OsStrExt;
                let blob =
                    String::from_utf8(git(&source, &["rev-parse", "HEAD:readme.txt"])).unwrap();
                let output = Command::new(git_path())
                    .current_dir(&source)
                    .args([
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        "100644",
                        blob.trim(),
                    ])
                    .arg(std::ffi::OsStr::from_bytes(b"name-\xff"))
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            _ => unreachable!(),
        }
        git(&source, &["commit", "-m", "unsupported execution input"]);
        let before = git(&source, &["show-ref"]);
        let output = temp.path().join("rejected");
        let error = intake(&source, &output, &git_path())
            .unwrap_err()
            .to_string();
        assert!(!output.exists(), "{form}");
        assert_eq!(git(&source, &["show-ref"]), before);
        if form == "non-utf8" {
            assert!(
                error.contains("6e616d652dff"),
                "lossless path required: {error}"
            );
        }
    }
}

#[test]
fn failed_git_probe_retains_exit_and_raw_non_utf8_output() {
    use std::os::unix::fs::PermissionsExt;
    let temp = temp();
    let verifier = temp.path().join("failed-git");
    fs::write(
        &verifier,
        b"#!/bin/sh\nprintf '\\377\\000out'\nprintf 'err\\000' >&2\nexit 9\n",
    )
    .unwrap();
    fs::set_permissions(&verifier, fs::Permissions::from_mode(0o700)).unwrap();
    // Another test forking at the moment the script was written holds its descriptor
    // until that child calls exec, and the kernel refuses to run a file open for
    // writing; the window is a few microseconds.
    let error = loop {
        let error = git_check(&verifier, temp.path()).unwrap_err().to_string();
        if !error.contains("Text file busy") {
            break error;
        }
        std::thread::yield_now();
    };
    assert!(error.contains("exit=Some(9)"), "{error}");
    assert!(error.contains("stdout_hex=ff006f7574"), "{error}");
    assert!(error.contains("stderr_hex=65727200"), "{error}");
}

#[test]
fn prepublication_content_checks_reject_control_paths_and_unsupported_modes_without_checkout() {
    let temp = temp();
    let mut odb = NativeOdb::open(temp.path().join("odb")).unwrap();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"ordinary");
    odb.put(oid, ObjectKind::Blob, b"ordinary").unwrap();
    odb.flush().unwrap();
    for (path, mode) in [
        ("nested/.GIT/config", 0o100644),
        (".alt/data", 0o100644),
        ("file", 0o160000),
    ] {
        let entries = BTreeMap::from([(
            path.into(),
            FileEntry {
                mode,
                oid: oid.to_string(),
            },
        )]);
        assert!(validate_files(&entries, &odb).is_err());
    }
    assert!(!temp.path().join("nested").exists());
}
