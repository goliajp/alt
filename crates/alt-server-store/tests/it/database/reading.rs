//! Reads of a repository while a push holds its row.
use super::*;

/// Another transaction holds the row of the repository, as a push does while its
/// objects are checked. The reads of the repository are answered meanwhile, with
/// what was committed before.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_read_of_a_repository_waits_for_no_push_to_it() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let row = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";
    held.query_one(row, &[]).unwrap();

    let mut reader = f.reopen();
    let (credential, key) = (f.credential.clone(), first.publication.key.clone());
    let (tell, told) = std::sync::mpsc::channel();
    let reading = std::thread::spawn(move || {
        let info = reader.repository_info(&credential, "repository").unwrap();
        tell.send(info.refs.len()).unwrap();
        let recorded = reader.query(&credential, &key).unwrap();
        tell.send(matches!(recorded, RequestObservation::Recorded { .. }) as usize)
            .unwrap();
        let read = reader.read_snapshot(&credential, "repository", 60_000, 1000);
        tell.send(read.unwrap().refs.len()).unwrap();
    });
    let wait = std::time::Duration::from_secs(20);
    let answers: Vec<_> = (0..3).map(|_| told.recv_timeout(wait)).collect();
    drop(held);
    reading.join().unwrap();
    assert_eq!(answers, [Ok(1), Ok(1), Ok(1)]);
}

/// Another transaction changes the row of a request and has not ended, as the
/// publication of it does. Who asks what became of the request is told what was
/// committed of it, and does not wait to be told that nobody knows.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn what_became_of_a_request_is_told_while_the_request_is_being_changed() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let touched = held
        .execute(
            "UPDATE alt_requests SET state=state WHERE repository='repository'",
            &[],
        )
        .unwrap();
    assert_eq!(touched, 1);

    let mut reader = f.reopen();
    let (credential, key) = (f.credential.clone(), first.publication.key.clone());
    let (tell, told) = std::sync::mpsc::channel();
    let asking = std::thread::spawn(move || {
        let recorded = reader.query(&credential, &key);
        let _ = tell.send(matches!(recorded, Ok(RequestObservation::Recorded { .. })));
        recorded.map(drop).map_err(|error| format!("{error:?}"))
    });
    let answer = told.recv_timeout(std::time::Duration::from_secs(20));
    held.commit().unwrap();
    let ended = asking.join().unwrap();
    assert_eq!((answer, ended), (Ok(true), Ok(())));
}

#[test]
#[ignore = "a measurement of the reads of a repository, for --nocapture; requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_time_of_the_reads_of_a_repository() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let key = first.publication.key.clone();
    for round in 0..5 {
        let began = std::time::Instant::now();
        for _ in 0..500 {
            f.store
                .repository_info(&f.credential, "repository")
                .unwrap();
            f.store.query(&f.credential, &key).unwrap();
        }
        println!("round {round}: 1000 reads in {:?}", began.elapsed());
    }
}

/// The credential a read is made with is changed by another transaction while the
/// read is under way: the read waits for the change, is begun again, and is
/// answered with what the change left. It does not fail for having been overtaken.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_read_overtaken_by_a_change_to_its_credential_is_begun_again() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let touched = held
        .execute("UPDATE alt_credentials SET expires_at=expires_at", &[])
        .unwrap();
    assert!(touched >= 1);

    let mut reader = f.reopen();
    let (credential, key) = (f.credential.clone(), first.publication.key.clone());
    let reading = std::thread::spawn(move || {
        let info = reader
            .repository_info(&credential, "repository")
            .map(|info| info.refs.len());
        let recorded = reader.query(&credential, &key);
        let recorded = recorded.map(|seen| matches!(seen, RequestObservation::Recorded { .. }));
        (
            info.map_err(|e| format!("{e:?}")),
            recorded.map_err(|e| format!("{e:?}")),
        )
    });
    // the read has begun and waits for the row of its credential
    let mut watcher = f.client();
    let waiting = "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE '%alt_credentials%'";
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while watcher.query_one(waiting, &[]).unwrap().get::<_, i64>(0) == 0 {
        assert!(
            std::time::Instant::now() < until,
            "the read never came to wait"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    held.commit().unwrap();
    assert_eq!(reading.join().unwrap(), (Ok(1), Ok(true)));
}

/// what the other reads of a repository answer, each told as it is answered
fn other_reads(
    mut reader: Store,
    credential: Credential,
    key: RequestKey,
    tell: std::sync::mpsc::Sender<&'static str>,
) {
    let scope = key.scope.clone();
    let told = |name: &'static str, answered: bool| {
        assert!(answered, "{name}");
        tell.send(name).unwrap();
    };
    let channels = reader.change_channels(&credential, &scope);
    told("change channels", channels.is_ok_and(|all| all.is_empty()));
    let events = reader.change_events(&credential, &scope, None);
    told(
        "change events",
        events.is_ok_and(|page| page.entries.is_empty()),
    );
    let event = reader.change_request_event(&credential, &key);
    told(
        "change request event",
        event.is_ok_and(|event| event.is_none()),
    );
    // a digest nothing has: refused, not waited for
    let event = reader.change_event(&credential, &scope, &"0".repeat(64));
    told("change event", event.is_err());
    let content = reader.open_published_content(&credential, &scope, &"0".repeat(64));
    told("published content", content.is_err());
    let access = reader.check_git_receive_access(&credential, "repository");
    told("receive access", access.is_ok());
    let inspection = reader.check_portable_inspection(&credential, &scope);
    told("portable inspection", inspection.is_ok());
    let root = RecordRoot {
        kind: RecordKind::Task,
        id: "0".repeat(64),
    };
    let bundles = reader.bundles_for_record(&credential, &scope, &root, None, 10);
    told("bundles", bundles.is_ok_and(|all| all.is_empty()));
    let record = reader.published_record(&credential, &scope, &root, 1024);
    told("published record", record.is_err());
}
const OTHER_READS: [&str; 9] = [
    "change channels",
    "change events",
    "change request event",
    "change event",
    "published content",
    "receive access",
    "portable inspection",
    "bundles",
    "published record",
];

/// The reads that are not of references or requests wait for no push either.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_other_reads_of_a_repository_wait_for_no_push_to_it() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let row = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";
    held.query_one(row, &[]).unwrap();

    let (reader, credential) = (f.reopen(), f.credential.clone());
    let key = first.publication.key.clone();
    let (tell, told) = std::sync::mpsc::channel();
    let reading = std::thread::spawn(move || other_reads(reader, credential, key, tell));
    let wait = std::time::Duration::from_secs(20);
    let answers: Vec<_> = (OTHER_READS.iter().map(|_| told.recv_timeout(wait).ok())).collect();
    held.commit().unwrap();
    reading.join().unwrap();
    assert_eq!(answers, OTHER_READS.map(Some));
}

/// On a Cloud instance another transaction holds the row every transaction there
/// locks, and the row of the repository: the reads are answered meanwhile. The
/// clock a read is held to is the one recorded, which a read moves on without
/// locking it for the length of the read.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_read_on_a_cloud_instance_waits_for_no_other_transaction() {
    let mut f = super::cloud::cloud();
    super::cloud::repository(&mut f);
    let scope = Scope {
        installation_id: "installation".into(),
        repository_instance_id: "repository".into(),
        recovery_epoch: 1,
    };
    let key = RequestKey {
        scope,
        actor_id: "owner".into(),
        request_id: "none".into(),
    };
    let high_water = |f: &Fixture| -> i64 {
        let recorded = "SELECT clock_high_water FROM alt_cloud_binding";
        f.client().query_one(recorded, &[]).unwrap().get(0)
    };
    // opened first: opening an instance is not a read
    let (mut reader, credential) = (f.reopen(), f.credential.clone());
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    held.batch_execute("SELECT 1 FROM alt_cloud_binding FOR UPDATE; SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE")
        .unwrap();

    let (tell, told) = std::sync::mpsc::channel();
    let reading = std::thread::spawn(move || {
        let info = reader.repository_info(&credential, "repository");
        tell.send("info").unwrap();
        info.unwrap();
        other_reads(reader, credential, key, tell);
    });
    let wait = std::time::Duration::from_secs(20);
    let first = told.recv_timeout(wait).ok();
    let answers: Vec<_> = (OTHER_READS.iter().map(|_| told.recv_timeout(wait).ok())).collect();
    held.commit().unwrap();
    reading.join().unwrap();
    assert_eq!(first, Some("info"));
    assert_eq!(answers, OTHER_READS.map(Some));

    // a read alone moves the recorded clock on, and is refused under a clock
    // that is behind what is recorded
    let before = high_water(&f);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let mut reader = f.reopen();
    reader.repository_info(&f.credential, "repository").unwrap();
    assert!(high_water(&f) >= before + 1000);
    let ahead = future() as i64;
    let set = "UPDATE alt_cloud_binding SET clock_high_water=$1";
    f.client().execute(set, &[&ahead]).unwrap();
    assert!(reader.repository_info(&f.credential, "repository").is_err());
    assert_eq!(high_water(&f), ahead);
}
