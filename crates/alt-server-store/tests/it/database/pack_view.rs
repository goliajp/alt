//! The references a view holds for each way of asking for them.
use super::*;
use alt_server_store::{ARRAY_TIPS, RefFilter, RefScope};

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_view_by_tips_holds_the_references_at_the_wanted_objects_and_tells_of_the_others() {
    let mut f = Fixture::new();
    let oid = |digit: char| digit.to_string().repeat(40);
    let rows = [
        ("refs/heads/a", oid('1')),
        ("refs/heads/b", oid('2')),
        ("refs/heads/same-as-a", oid('1')),
        ("refs/tags/v1", oid('3')),
        ("refs/tags/v2", oid('2')),
    ];
    let mut client = f.client();
    for (name, oid) in &rows {
        client
            .execute(
                "INSERT INTO alt_refs(repository,name,oid) VALUES('repository',$1,$2)",
                &[&name.to_string(), oid],
            )
            .unwrap();
    }
    // the same repository name elsewhere never shows through
    client
        .batch_execute(&format!(
            "INSERT INTO alt_repositories(id,recovery_epoch,object_format,quota_bytes) VALUES('other',1,'sha1',0);
             INSERT INTO alt_refs(repository,name,oid) VALUES('other','refs/heads/elsewhere','{}')",
            oid('9')
        ))
        .unwrap();
    let absent = oid('f');
    let cases: [(&[String], bool); 9] = [
        (&[], false),
        (&[], true),
        (&[oid('1')], false),
        (&[oid('1'), oid('1'), oid('1')], false),
        (std::slice::from_ref(&absent), false),
        (&[absent.clone(), oid('2')], true),
        (&[oid('9')], false),
        (&[oid('1'), oid('2'), oid('3')], false),
        (
            &[oid('3'), absent.clone(), oid('1'), oid('2'), oid('2')],
            true,
        ),
    ];
    // ids no reference has, to reach the lengths either side of where the statement changes
    let padding: Vec<String> = (0..=ARRAY_TIPS)
        .map(|index| format!("e{index:039x}"))
        .collect();
    for (wants, tags) in cases {
        for length in [wants.len(), ARRAY_TIPS, ARRAY_TIPS + 1] {
            let mut padded = wants.to_vec();
            padded.extend_from_slice(&padding[..length - wants.len()]);
            let view = f
                .store
                .pack_view_with(
                    &f.credential,
                    "repository",
                    RefFilter::Tips {
                        wants: &padded,
                        tags,
                    },
                )
                .unwrap();
            let expected: Vec<(&str, String)> = rows
                .iter()
                .filter(|(name, oid)| {
                    wants.contains(oid) || (tags && name.starts_with("refs/tags/"))
                })
                .map(|(name, oid)| (*name, oid.clone()))
                .collect();
            let held: Vec<(&str, String)> = view
                .refs
                .iter()
                .map(|(name, oid)| (name, oid.to_string()))
                .collect();
            assert_eq!(held, expected, "wants {wants:?} of {length} tags {tags}");
            assert_eq!(
                view.ref_scope,
                RefScope::Filtered {
                    other_tips: rows.iter().any(|(_, oid)| !wants.contains(oid)),
                },
                "wants {wants:?} of {length} tags {tags}"
            );
        }
    }
}

/// The scans of `alt_refs` that `asked` causes on a connection of its own, which
/// reports them as it ends.
fn reference_scans(f: &Fixture, asked: impl FnOnce(&mut Store)) -> i64 {
    let mut client = f.client();
    let scans = "SELECT coalesce(seq_scan,0)+coalesce(idx_scan,0) FROM pg_stat_user_tables WHERE relname='alt_refs'";
    let backends = "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database()";
    let mut store = f.reopen();
    let open: i64 = client.query_one(backends, &[]).unwrap().get(0);
    let before: i64 = client.query_one(scans, &[]).unwrap().get(0);
    asked(&mut store);
    drop(store);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while client.query_one(backends, &[]).unwrap().get::<_, i64>(0) != open - 1 {
        assert!(std::time::Instant::now() < deadline, "the connection stays");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    client.query_one(scans, &[]).unwrap().get::<_, i64>(0) - before
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_references_are_not_read_for_a_caller_who_may_not_read_the_repository() {
    let f = Fixture::new();
    let oid = "1".repeat(40);
    f.client()
        .batch_execute(&format!(
            "INSERT INTO alt_refs(repository,name,oid) VALUES('repository','refs/heads/a','{oid}');
             INSERT INTO alt_repositories(id,recovery_epoch,object_format,quota_bytes) VALUES('other',1,'sha1',0);
             INSERT INTO alt_refs(repository,name,oid) VALUES('other','refs/heads/elsewhere','{oid}')"
        ))
        .unwrap();
    let prefixes = ["refs/".to_owned()];
    let wants = [oid.clone()];
    let many: Vec<String> = (0..=ARRAY_TIPS).map(|n| format!("e{n:039x}")).collect();
    let every_way = |store: &mut Store, token: &str, repository: &str| -> usize {
        let ways = [
            RefFilter::All,
            RefFilter::Bounded,
            RefFilter::Prefixes(&prefixes),
            RefFilter::Tips {
                wants: &wants,
                tags: true,
            },
            RefFilter::Tips {
                wants: &many,
                tags: true,
            },
        ];
        let opened = ways.map(|way| store.git_open(token, repository, way));
        opened.iter().filter(|view| view.is_ok()).count()
    };

    // who holds a grant has them read, each way
    let read = reference_scans(&f, |store| {
        assert_eq!(every_way(store, &f.token, "repository"), 5);
    });
    assert!(read >= 5, "{read} scans reported");
    // a token nobody was given, and one with no grant on the repository asked for
    let unknown = alt_browser_auth::token();
    let refused = reference_scans(&f, |store| {
        assert_eq!(every_way(store, &unknown, "repository"), 0);
        assert_eq!(every_way(store, &f.token, "other"), 0);
    });
    assert_eq!(refused, 0);
}

/// A token of an agent takes the path that locks the repository's row. There too a
/// repository the token has no grant on, and any repository once the token is
/// revoked, has none of its references read.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_references_are_not_read_for_an_agent_that_may_not_read_the_repository() {
    let mut f = Fixture::new();
    let oid = "1".repeat(40);
    f.client()
        .batch_execute(&format!(
            "INSERT INTO alt_refs(repository,name,oid) VALUES('repository','refs/heads/main','{oid}');
             INSERT INTO alt_repositories(id,recovery_epoch,object_format,quota_bytes) VALUES('other',1,'sha1',0);
             INSERT INTO alt_refs(repository,name,oid) VALUES('other','refs/heads/elsewhere','{oid}')"
        ))
        .unwrap();
    let request = super::delegation::request();
    let agent = request.access_token.clone();
    f.store.issue_agent(&f.credential, &request).unwrap();

    let read = reference_scans(&f, |store| {
        let (_, view) = store
            .git_open(&agent, "repository", RefFilter::All)
            .unwrap();
        assert_eq!(view.refs.len(), 1);
    });
    assert!(read >= 1, "{read} scans reported");
    let elsewhere = reference_scans(&f, |store| {
        assert!(store.git_open(&agent, "other", RefFilter::All).is_err());
    });
    assert_eq!(elsewhere, 0);
    f.client()
        .batch_execute("UPDATE alt_credentials SET revoked=true,revoked_at=0 WHERE token_digest IN (SELECT token_digest FROM alt_delegations)")
        .unwrap();
    let revoked = reference_scans(&f, |store| {
        assert!(
            store
                .git_open(&agent, "repository", RefFilter::All)
                .is_err()
        );
    });
    assert_eq!(revoked, 0);
}
