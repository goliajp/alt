//! import → export round trips on fixture repositories: the exported
//! .git must satisfy git itself (fsck) and answer identically to the
//! source repository (refs, HEAD, full history, config).

use std::path::Path;
use std::process::{Command, Output};

use alt_export::{ExportError, export_git};
use alt_repo::Repository;

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap()
}

fn git_ok(repo: &Path, args: &[&str]) -> Vec<u8> {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} in {repo:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// import → export, then compare the exported repo against the source
/// through git's own eyes.
fn cycle(source: &Path) {
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let repo = Repository::discover(source).unwrap();
    alt_import::import_git(&repo, &alt_dir, "test/cycle", 1).unwrap();

    let out_root = tempfile::tempdir().unwrap();
    let target = out_root.path().join("exported");
    let report = export_git(&alt_dir, &target).unwrap();
    eprintln!("cycle {source:?}: {report:?}");
    assert!(report.objects > 0);
    assert!(report.head);
    assert!(report.refs > 0, "export wrote no refs");

    // git is the referee
    let fsck = git(&target, &["fsck", "--strict"]);
    assert!(
        fsck.status.success(),
        "git fsck in {target:?}: {}{}",
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr)
    );

    // semantic equality, source vs exported (S2's deferred check included)
    for args in [
        &["for-each-ref"][..],
        &["symbolic-ref", "HEAD"],
        &["rev-parse", "HEAD"],
        &["log", "--pretty=raw", "--all"],
    ] {
        let want = git_ok(source, args);
        let got = git_ok(&target, args);
        assert_eq!(
            String::from_utf8_lossy(&want),
            String::from_utf8_lossy(&got),
            "{args:?} differs between source and export"
        );
    }

    // contract 2: config returns byte-identical, except that the export
    // writes files-backend refs, so a refstorage extension is dropped
    let want = std::fs::read(source.join(".git/config")).unwrap();
    let want = String::from_utf8(want)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().to_lowercase().starts_with("refstorage"))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let got = std::fs::read(target.join(".git/config")).unwrap();
    assert_eq!(want, String::from_utf8(got).unwrap());
}

#[test]
fn fixture_cycles_across_formats() {
    for (object_format, ref_format) in
        [("sha1", "files"), ("sha256", "files"), ("sha1", "reftable")]
    {
        let source = tempfile::tempdir().unwrap();
        alt_testutil::make_repo_opts(source.path(), object_format, ref_format);
        cycle(source.path());
        // and again from a fully packed source
        alt_testutil::pack_repo(source.path());
        cycle(source.path());
    }
}

#[test]
fn export_refuses_a_non_empty_target() {
    let source = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(source.path(), "sha1");
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let repo = Repository::discover(source.path()).unwrap();
    alt_import::import_git(&repo, &alt_dir, "test/cycle", 1).unwrap();

    let out_root = tempfile::tempdir().unwrap();
    std::fs::write(out_root.path().join("occupied"), "x").unwrap();
    let err = match export_git(&alt_dir, out_root.path()) {
        Ok(_) => panic!("non-empty target must be refused"),
        Err(e) => e,
    };
    assert!(matches!(err, ExportError::TargetNotEmpty(_)), "got {err:?}");
}
