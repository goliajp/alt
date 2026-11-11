use alt_pack_segment::{Limits, Segment, SegmentSet, ingest};
use std::{fs, io::Write, path::Path, process::Command, sync::Arc};

const LIMITS: Limits = Limits {
    objects: 1 << 20,
    object_bytes: 1 << 30,
    inflated_bytes: 1 << 32,
    delta_depth: 64,
};

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.test")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn git_in(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_owned()
}

fn oid(hex: &str) -> [u8; 20] {
    let mut out = [0; 20];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
    }
    out
}

/// a repository with `commits` commits, each rewriting one large-ish file and adding a small one
fn repository(dir: &Path, commits: usize) {
    git(dir, &["init", "-q", "--initial-branch=main"]);
    fs::create_dir(dir.join("src")).unwrap();
    for index in 0..commits {
        let body: String = (0..400)
            .map(|line| {
                format!(
                    "line {line} of revision {}\n",
                    if line % 37 == index % 37 { index } else { 0 }
                )
            })
            .collect();
        fs::write(dir.join("src/big.txt"), body).unwrap();
        fs::write(
            dir.join(format!("src/file-{index}.txt")),
            format!("file {index}\n"),
        )
        .unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", &format!("revision {index}")]);
    }
    git(dir, &["tag", "-a", "v1", "-m", "tagged"]);
}

fn segment(dir: &Path, name: &str, pack: &[u8], existing: &SegmentSet) -> Arc<Segment> {
    let ingested = ingest(pack, existing, &LIMITS).unwrap();
    let path = dir.join(name);
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(pack).unwrap();
    ingested.write_index(&mut file).unwrap();
    Arc::new(Segment::open(&path).unwrap())
}

fn objects(dir: &Path, range: &[&str]) -> Vec<String> {
    let mut args = vec!["rev-list", "--objects"];
    args.extend_from_slice(range);
    text(git(dir, &args))
        .lines()
        .map(|line| line[..40].to_owned())
        .collect()
}

fn check_objects(dir: &Path, set: &SegmentSet, ids: &[String]) {
    for id in ids {
        let (kind, data) = set.read(&oid(id)).unwrap();
        assert_eq!(kind.name(), text(git(dir, &["cat-file", "-t", id])));
        assert_eq!(
            data,
            git(dir, &["cat-file", kind.name(), id]),
            "object {id}"
        );
    }
}

#[test]
fn whole_repository_round_trips() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    repository(&source, 40);
    git(&source, &["repack", "-adq", "--depth=20", "--window=20"]);
    let pack = git_in(
        &source,
        &[
            "pack-objects",
            "--all",
            "--stdout",
            "-q",
            "--delta-base-offset",
        ],
        b"",
    );
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "one.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    let ids = objects(&source, &["--all"]);
    assert_eq!(set.objects() as usize, ids.len());
    check_objects(&source, &set, &ids);
    assert!(set.verbatim());

    let mut sent = Vec::new();
    set.write_all(&mut sent).unwrap();
    assert_eq!(sent, pack);

    let head = oid(&text(git(&source, &["rev-parse", "HEAD"])));
    assert_eq!(set.commit(&head).unwrap().generation, 40);
    let tag = oid(&text(git(&source, &["rev-parse", "v1"])));
    let selection = set.select(&[tag], &[]).unwrap();
    assert_eq!(selection.len(), ids.len());
    let mut sent = Vec::new();
    set.write_selected(&selection, false, &mut sent).unwrap();
    let target = work.path().join("target");
    fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q", "--bare"]);
    git_in(
        &target,
        &["index-pack", "--stdin", "--strict", "--fix-thin"],
        &sent,
    );
    git(
        &target,
        &[
            "update-ref",
            "refs/tags/v1",
            &text(git(&source, &["rev-parse", "v1"])),
        ],
    );
    git(&target, &["fsck", "--strict", "--full"]);
}

#[test]
fn thin_increment_and_fetch_selection() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    repository(&source, 10);
    let old = text(git(&source, &["rev-parse", "HEAD"]));
    let first = git_in(
        &source,
        &[
            "pack-objects",
            "--all",
            "--stdout",
            "-q",
            "--delta-base-offset",
        ],
        b"",
    );
    let base = SegmentSet::new(vec![segment(
        work.path(),
        "one.seg",
        &first,
        &SegmentSet::default(),
    )]);

    fs::write(
        source.join("src/big.txt"),
        fs::read_to_string(source.join("src/big.txt"))
            .unwrap()
            .replace("line 5 ", "line five "),
    )
    .unwrap();
    git(&source, &["commit", "-qam", "increment"]);
    let new = text(git(&source, &["rev-parse", "HEAD"]));
    let thin = git_in(
        &source,
        &[
            "pack-objects",
            "--thin",
            "--stdout",
            "-q",
            "--revs",
            "--delta-base-offset",
        ],
        format!("{new}\n^{old}\n").as_bytes(),
    );
    let second = segment(work.path(), "two.seg", &thin, &base);
    let both = SegmentSet::new(vec![base.segments()[0].clone(), second]);
    let added = objects(&source, &[&new, &format!("^{old}")]);
    check_objects(&source, &both, &added);

    // a pack that references something absent is refused
    let dangling = git_in(
        &source,
        &["pack-objects", "--stdout", "-q", "--revs"],
        format!("{new}\n^{old}\n").as_bytes(),
    );
    assert!(ingest(&dangling, &SegmentSet::default(), &LIMITS).is_err());

    let selection = both.select(&[oid(&new)], &[oid(&old)]).unwrap();
    let mut wanted: Vec<_> = added.clone();
    wanted.sort();
    assert_eq!(selection.len(), wanted.len());
    let mut sent = Vec::new();
    both.write_selected(&selection, true, &mut sent).unwrap();
    let target = work.path().join("target");
    fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q", "--bare"]);
    git_in(&target, &["index-pack", "--stdin", "--strict"], &first);
    git_in(
        &target,
        &["index-pack", "--stdin", "--strict", "--fix-thin"],
        &sent,
    );
    git(&target, &["update-ref", "refs/heads/main", &new]);
    git(&target, &["fsck", "--strict", "--full"]);

    // both segments resent as one pack
    let mut sent = Vec::new();
    both.write_all(&mut sent).unwrap();
    let whole = work.path().join("whole");
    fs::create_dir(&whole).unwrap();
    git(&whole, &["init", "-q", "--bare"]);
    git_in(&whole, &["index-pack", "--stdin", "--strict"], &sent);
    git(&whole, &["update-ref", "refs/heads/main", &new]);
    git(&whole, &["fsck", "--strict", "--full"]);
}

#[test]
fn corruption_is_detected() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    repository(&source, 3);
    let pack = git_in(
        &source,
        &[
            "pack-objects",
            "--all",
            "--stdout",
            "-q",
            "--delta-base-offset",
        ],
        b"",
    );
    let mut broken = pack.clone();
    let middle = broken.len() / 2;
    broken[middle] ^= 1;
    assert!(ingest(&broken, &SegmentSet::default(), &LIMITS).is_err());

    segment(work.path(), "one.seg", &pack, &SegmentSet::default());
    let path = work.path().join("one.seg");
    let mut bytes = fs::read(&path).unwrap();
    bytes[40] ^= 1;
    fs::write(&path, &bytes).unwrap();
    let set = SegmentSet::new(vec![Arc::new(Segment::open(&path).unwrap())]);
    let failed = objects(&source, &["--all"])
        .iter()
        .filter(|id| set.read(&oid(id)).is_err())
        .count();
    assert!(failed >= 1);
    let last = bytes.len() - 100;
    bytes[last] ^= 1;
    fs::write(&path, &bytes).unwrap();
    assert!(Segment::open(&path).is_err());
}

#[test]
fn selection_with_nested_tags_notes_and_symlinks() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    repository(&source, 2);
    std::os::unix::fs::symlink("src/big.txt", source.join("link")).unwrap();
    fs::write(source.join("asset.bin"), [0u8, 255, 1, 0, 13, 10, 200]).unwrap();
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-q", "-m", "more"]);
    git(&source, &["tag", "-a", "v2", "v1", "-m", "nested"]);
    git(&source, &["notes", "add", "-m", "facts", "HEAD"]);
    let pack = git_in(
        &source,
        &[
            "pack-objects",
            "--all",
            "--stdout",
            "-q",
            "--delta-base-offset",
        ],
        b"",
    );
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "one.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    let wants: Vec<_> = ["HEAD", "v1", "v2"]
        .iter()
        .map(|name| oid(&text(git(&source, &["rev-parse", name]))))
        .collect();
    let selection = set.select(&wants, &[]).unwrap();
    let expected = objects(&source, &["HEAD", "v1", "v2"]);
    assert_eq!(selection.len(), expected.len());
    let mut sent = Vec::new();
    set.write_selected(&selection, true, &mut sent).unwrap();
    let target = work.path().join("target");
    fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q", "--bare"]);
    git_in(&target, &["index-pack", "--stdin", "--strict"], &sent);
}
