//! A repository whose history holds a name `git fsck` refuses, which the
//! server holds as a release that took it left it: a task sent from it is
//! not refused by the client, which does not know what the server holds,
//! and the server takes it, the history being its own. A client receiving
//! the task refuses it where the history is new to its repository, and takes
//! it where the repository holds it.
use super::*;

/// Every object of the Git repository `git` as a live segment of the
/// repository of `server`, and its refs as they are there.
fn planted(server: &Server, git: &Path) {
    use alt_pack_segment::Kind;
    let read = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(git)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        output.stdout
    };
    let listed = read(&[
        "cat-file",
        "--batch-all-objects",
        "--batch-check=%(objectname) %(objecttype)",
    ]);
    let listed = String::from_utf8(listed).unwrap();
    let mut objects = Vec::new();
    for line in listed.lines() {
        let (oid, named) = line.split_once(' ').unwrap();
        let kind = match named {
            "blob" => Kind::Blob,
            "tree" => Kind::Tree,
            "commit" => continue,
            other => panic!("{other}"),
        };
        objects.push((kind, read(&["cat-file", named, oid])));
    }
    // a commit after its parents
    let commits = read(&["rev-list", "--reverse", "--topo-order", "--all"]);
    for oid in String::from_utf8(commits).unwrap().lines() {
        objects.push((Kind::Commit, read(&["cat-file", "commit", oid])));
    }
    let none = alt_pack_segment::SegmentSet::default();
    let out = std::io::Cursor::new(Vec::new());
    let mut builder = alt_pack_segment::Builder::new(out, &none, objects.len() as u64).unwrap();
    for (kind, data) in &objects {
        builder.add(*kind, data).unwrap();
    }
    let built = builder.finish().unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    let bytes = built.out.into_inner();
    std::fs::write(server.store.pack_path("repository", id).unwrap(), &bytes).unwrap();
    let mut config: alt_pg::Config = server.admin_dsn.parse().unwrap();
    config.set_dbname(&server.database);
    let mut client = config.connect().unwrap();
    client
        .execute(
            "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state) VALUES('repository',$1,1,$2,$3,0,'live')",
            &[&id, &(built.objects as i64), &(bytes.len() as i64)],
        )
        .unwrap();
    let refs = String::from_utf8(read(&["for-each-ref", "--format=%(objectname) %(refname)"]));
    for line in refs.unwrap().lines() {
        let (oid, name) = line.split_once(' ').unwrap();
        client
            .execute(
                "INSERT INTO alt_refs(repository,name,oid) VALUES('repository',$1,$2)",
                &[&name, &oid],
            )
            .unwrap();
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP, real Git and disk-backed runner; exercised by closing gate"]
fn a_task_of_a_history_with_a_name_git_fsck_refuses_is_taken_and_received_where_it_is_held() {
    let server = Server::new();
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = alt_os::temp::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let scratch = alt_os::temp::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    let alice = root.path().join("alice");
    let aw = root.path().join("alice-work");
    // the history is made by git, to which `ALT~1` is a name like any
    // other, and imported: alt neither stages nor checks out a path that
    // some file systems read as `.alt`, so the name is left out of the
    // commit a workspace is made at, as git leaves `git~1` out
    let alice_git = root.path().join("alice-git");
    std::fs::create_dir(&alice_git).unwrap();
    let identity = ["-c", "user.name=alice", "-c", "user.email=alice@test"];
    let commit = |message: &str| {
        let args = [&identity[..], &["commit", "-q", "-m", message]].concat();
        git(&alice_git, &server.token, &args);
    };
    git(&alice_git, &server.token, &["init", "-q", "-b", "main"]);
    base::write(&alice_git);
    std::fs::create_dir(alice_git.join("ALT~1")).unwrap();
    std::fs::write(alice_git.join("ALT~1/kept"), "kept\n").unwrap();
    git(&alice_git, &server.token, &["add", "-A"]);
    commit("base");
    git(&alice_git, &server.token, &["rm", "-q", "-r", "ALT~1"]);
    commit("leave the name out");
    ok(
        &alice_git,
        &server.token,
        &["import", alice.to_str().unwrap()],
    );
    let baseline_git = root.path().join("baseline-git");
    let exported = baseline_git.to_str().unwrap();
    ok(&alice, &server.token, &["export", exported]);
    planted(&server, &baseline_git);

    ok(
        &alice,
        &server.token,
        &["workspace", "add", "work", aw.to_str().unwrap()],
    );
    let tools = json!([{"name":"bash","path":"/bin/bash","version_args":["--version"]}]);
    let environment = json!({"PATH":"/usr/bin:/bin"});
    let probe = value(
        scratch.path(),
        &server.token,
        &[
            "runner",
            "probe",
            "--bindings",
            &tools.to_string(),
            "--environment",
            &environment.to_string(),
            "--json",
        ],
    );
    let definition = json!({"purpose":"a task over a history the server holds","authority":"owner","allowed_paths":["source.txt"],"auxiliary":[],"exclusions":[],"validation":{"commands":[["bash","gate.sh"]],"cwd":".","environment":{"values":environment,"complete":true},"tools":probe["tools"],"external":[],"validation":{"outputs":[]}}});
    let task = value(
        &aw,
        &server.token,
        &[
            "task",
            "create",
            "--definition",
            &definition.to_string(),
            "--target",
            "refs/heads/main",
            "--json",
        ],
    );
    let task_id = string(&task, "task_id");
    std::fs::write(aw.join("source.txt"), "changed\n").unwrap();
    ok(&aw, &server.token, &["add", "source.txt"]);
    let candidate = value(
        &aw,
        &server.token,
        &["candidate", "create", task_id, "--json"],
    );
    let profile = json!({"kind":"local","scratch":scratch.path(),"tools":tools,"timeout_ms":5000})
        .to_string();
    let evidence = value(
        &aw,
        &server.token,
        &[
            "evidence",
            "run",
            string(&candidate, "candidate_id"),
            "--profile",
            &profile,
            "--json",
        ],
    );
    assert_eq!(evidence["validity"]["eligible"], true);
    connect(&aw, &server, &server.token, "origin");
    let plan = json!({"selection":{"roots":[{"kind":"evidence","id":evidence["evidence_id"]}],"companions":[{"kind":"task-baseline","id":task["baseline_id"]}],"omissions":[],"attestations":[]},"refs":[]});
    let previewed = preview(
        &aw,
        &server.token,
        "origin",
        &plan,
        &root.path().join("plan.json"),
    );
    let id = string(&previewed, "publication_id");
    let published = value(
        &aw,
        &server.token,
        &["task", "remote", "publish", id, "--json"],
    );
    assert_eq!(published["state"], "committed", "{published}");

    let digest = string(&previewed, "request_digest");
    let receive = [
        "task",
        "remote",
        "receive",
        "origin",
        "--kind",
        "evidence",
        "--id",
        string(&evidence, "evidence_id"),
        "--publication",
        digest,
        "--json",
    ];
    // a repository the history is new to
    let bob = root.path().join("bob");
    std::fs::create_dir(&bob).unwrap();
    ok(&bob, &server.bob, &["init", "."]);
    connect(&bob, &server, &server.bob, "origin");
    let refused = run(&bob, &server.bob, &receive);
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{said}");
    assert!(said.contains("hasDotalt: contains '.alt'"), "{said}");
    let base = String::from_utf8(ok(&alice, &server.token, &["rev-parse", "HEAD"]).stdout);
    let base = base.unwrap();
    let held = run(&bob, &server.bob, &["cat-file", "-t", base.trim()]);
    assert!(!held.status.success(), "{held:?}");
    // the repository the history is from
    let received = value(&aw, &server.token, &receive);
    assert_eq!(received["state"], "received", "{received}");
    // the repository holds the commit with the name, which is not checked out
    let refused = run(&alice, &server.token, &["switch", "-c", "held", "HEAD~1"]);
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{said}");
    assert!(said.contains("invalid path 'ALT~1/kept'"), "{said}");
}
